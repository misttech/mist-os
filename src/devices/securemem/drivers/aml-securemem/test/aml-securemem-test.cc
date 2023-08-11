// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.tee/cpp/wire.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/c/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/env.h>
#include <lib/fpromise/result.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/resource.h>
#include <zircon/limits.h>

#include <fbl/array.h>
#include <zxtest/zxtest.h>

#include "device.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

class FakeSysmem : public ddk::SysmemProtocol<FakeSysmem> {
 public:
  FakeSysmem() : proto_({&sysmem_protocol_ops_, this}) {}

  const sysmem_protocol_t* proto() const { return &proto_; }

  zx_status_t SysmemConnect(zx::channel allocator2_request) {
    // Currently, do nothing
    return ZX_OK;
  }

  zx_status_t SysmemRegisterHeap(uint64_t heap, zx::channel heap_connection) {
    // Currently, do nothing
    return ZX_OK;
  }

  zx_status_t SysmemRegisterSecureMem(zx::channel tee_connection) {
    // Stash the tee_connection_ so the channel can stay open long enough to avoid a potentially
    // confusing error message during the test.
    tee_connection_ = std::move(tee_connection);
    return ZX_OK;
  }

  zx_status_t SysmemUnregisterSecureMem() {
    // Currently, do nothing
    return ZX_OK;
  }

 private:
  sysmem_protocol_t proto_;
  zx::channel tee_connection_;
};

// We cover the code involved in supporting non-VDEC secure memory and VDEC secure memory in
// sysmem-test, so this fake doesn't really need to do much yet.
class FakeTee : public fidl::WireServer<fuchsia_hardware_tee::DeviceConnector> {
 public:
  void ConnectToApplication(ConnectToApplicationRequestView request,
                            ConnectToApplicationCompleter::Sync& completer) override {
    // Currently, do nothing
    //
    // We don't rely on the tee_app_request channel sticking around for these tests.  See
    // sysmem-test for a test that exercises the tee_app_request channel.
  }

  void ConnectToDeviceInfo(ConnectToDeviceInfoRequestView request,
                           ConnectToDeviceInfoCompleter::Sync& completer) override {}
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

class AmlogicSecureMemTest : public zxtest::Test {
 protected:
  AmlogicSecureMemTest()
      : tee_server_loop_(fdf::Dispatcher::GetCurrent()),
        outgoing_(component::OutgoingDirectory(tee_server_loop_->async_dispatcher())) {
    fake_pdev::FakePDevFidl::Config config;
    config.use_fake_bti = true;

    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(outgoing_endpoints);
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(std::move(config));
      ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
          infra->pdev_server.GetInstanceHandler()));
      ASSERT_OK(infra->outgoing.Serve(std::move(server)));
    });
    ASSERT_NO_FATAL_FAILURE();
    root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                          std::move(outgoing_endpoints->client), "pdev");
    root_->AddProtocol(ZX_PROTOCOL_SYSMEM, sysmem_.proto()->ops, sysmem_.proto()->ctx, "sysmem");

    auto device_handler = [this](fidl::ServerEnd<fuchsia_hardware_tee::DeviceConnector> request) {
      fidl::BindServer(tee_server_loop_->async_dispatcher(), std::move(request), &tee_);
    };
    fuchsia_hardware_tee::Service::InstanceHandler handler(
        {.device_connector = std::move(device_handler)});

    auto service_result = outgoing_.AddService<fuchsia_hardware_tee::Service>(std::move(handler));
    ZX_ASSERT(service_result.is_ok());

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    ZX_ASSERT(outgoing_.Serve(std::move(endpoints->server)).is_ok());
    root_->AddFidlService(fuchsia_hardware_tee::Service::Name, std::move(endpoints->client), "tee");

    // We initialize this in a dispatcher thread so that fdf_dispatcher_get_current_dispatcher
    // works. This dispatcher isn't actually used in the test.
    fdf_env_register_driver_entry(reinterpret_cast<void*>(0x12345678));
    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        {}, "aml-securemem-test", fit::bind_member(this, &AmlogicSecureMemTest::ShutdownHandler));
    fdf_env_register_driver_exit();
    ASSERT_OK(dispatcher.status_value());
    dispatcher_ = *std::move(dispatcher);

    libsync::Completion completion;
    async::PostTask(dispatcher_.async_dispatcher(), [&]() {
      ASSERT_OK(amlogic_secure_mem::AmlogicSecureMemDevice::Create(nullptr, parent()));
      completion.Signal();
    });
    completion.Wait();
    ASSERT_EQ(root_->child_count(), 1);
    auto child = root_->GetLatestChild();
    dev_ = child->GetDeviceContext<amlogic_secure_mem::AmlogicSecureMemDevice>();
  }

  void TearDown() override {
    // For now, we use DdkSuspend(mexec) partly to cover DdkSuspend(mexec) handling, and
    // partly because it's the only way of cleaning up safely that we've implemented so far, as
    // aml-securemem doesn't yet implement DdkUnbind() - and arguably it doesn't really need to
    // given what aml-securemem is.

    async::PostTask(dispatcher_.async_dispatcher(), [&]() {
      dev()->zxdev()->SuspendNewOp(DEV_POWER_STATE_D3COLD, false, DEVICE_SUSPEND_REASON_MEXEC);
    });

    ASSERT_OK(dev()->zxdev()->WaitUntilSuspendReplyCalled());

    // Destroy the driver object in the dispatcher context.
    libsync::Completion destroy_completion;
    async::PostTask(dispatcher_.async_dispatcher(), [&]() {
      root_ = nullptr;
      destroy_completion.Signal();
    });
    destroy_completion.Wait();

    dispatcher_.ShutdownAsync();
    shutdown_completion_.Wait();
  }

  void ShutdownHandler(fdf_dispatcher_t* dispatcher) {
    ASSERT_EQ(dispatcher, dispatcher_.get());
    shutdown_completion_.Signal();
  }

  zx_device_t* parent() { return root_.get(); }

  amlogic_secure_mem::AmlogicSecureMemDevice* dev() { return dev_; }

 private:
  fdf::Unowned<fdf::Dispatcher> tee_server_loop_;
  // TODO(fxb/124464): Migrate test to use dispatcher integration.
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED();
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  FakeSysmem sysmem_;
  FakeTee tee_;
  amlogic_secure_mem::AmlogicSecureMemDevice* dev_;
  fdf::Dispatcher dispatcher_;

  component::OutgoingDirectory outgoing_;
  libsync::Completion shutdown_completion_;
};

TEST_F(AmlogicSecureMemTest, GetSecureMemoryPhysicalAddressBadVmo) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ZX_PAGE_SIZE, 0, &vmo));

  ASSERT_TRUE(dev()->GetSecureMemoryPhysicalAddress(std::move(vmo)).is_error());
}
