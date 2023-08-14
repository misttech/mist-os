// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.tee/cpp/wire.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
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

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem> {
 public:
  void ConnectServer(ConnectServerRequestView request,
                     ConnectServerCompleter::Sync& completer) override {
    // Currently, do nothing
  }

  void RegisterHeap(RegisterHeapRequestView request,
                    RegisterHeapCompleter::Sync& completer) override {
    // Currently, do nothing
  }

  void RegisterSecureMem(RegisterSecureMemRequestView request,
                         RegisterSecureMemCompleter::Sync& completer) override {
    // Stash the tee_connection_ so the channel can stay open long enough to avoid a potentially
    // confusing error message during the test.
    tee_connection_ = std::move(request->secure_mem_connection);
  }

  void UnregisterSecureMem(UnregisterSecureMemCompleter::Sync& completer) override {
    // Currently, do nothing
    completer.ReplySuccess();
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fuchsia_hardware_sysmem::Service::InstanceHandler CreateInstanceHandler() {
    return fuchsia_hardware_sysmem::Service::InstanceHandler({
        .sysmem = sysmem_bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                                 fidl::kIgnoreBindingClosure),
        .allocator_v1 = [](fidl::ServerEnd<fuchsia_sysmem::Allocator> request) {},
        .allocator = [](fidl::ServerEnd<fuchsia_sysmem2::Allocator> request) {},
    });
  }

 private:
  fidl::ClientEnd<fuchsia_sysmem::SecureMem> tee_connection_;
  fidl::ServerBindingGroup<fuchsia_hardware_sysmem::Sysmem> sysmem_bindings_;
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

  fuchsia_hardware_tee::Service::InstanceHandler CreateInstanceHandler() {
    return fuchsia_hardware_tee::Service::InstanceHandler(
        {.device_connector = bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                                     fidl::kIgnoreBindingClosure)});
  }

 private:
  fidl::ServerBindingGroup<fuchsia_hardware_tee::DeviceConnector> bindings_;
};

class AmlogicSecureMemTest : public zxtest::Test {
 protected:
  AmlogicSecureMemTest() {
    ASSERT_OK(incoming_loop_.StartThread("incoming"));

    // Create pdev fragment
    fake_pdev::FakePDevFidl::Config config;
    config.use_fake_bti = true;
    pdev_.SyncCall(&fake_pdev::FakePDevFidl::SetConfig, std::move(config));
    auto pdev_handler = pdev_.SyncCall(&fake_pdev::FakePDevFidl::GetInstanceHandler,
                                       async_patterns::PassDispatcher);
    auto pdev_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(pdev_endpoints.is_ok());
    root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                          std::move(pdev_endpoints->client), "pdev");

    // Create sysmem fragment
    auto sysmem_handler = sysmem_.SyncCall(&FakeSysmem::CreateInstanceHandler);
    auto sysmem_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(sysmem_endpoints.is_ok());
    root_->AddFidlService(fuchsia_hardware_sysmem::Service::Name,
                          std::move(sysmem_endpoints->client), "sysmem");

    // Create tee fragment
    auto tee_handler = tee_.SyncCall(&FakeTee::CreateInstanceHandler);
    auto tee_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(tee_endpoints.is_ok());
    root_->AddFidlService(fuchsia_hardware_tee::Service::Name, std::move(tee_endpoints->client),
                          "tee");

    outgoing_.SyncCall([pdev_server = std::move(pdev_endpoints->server),
                        sysmem_server = std::move(sysmem_endpoints->server),
                        tee_server = std::move(tee_endpoints->server),
                        pdev_handler = std::move(pdev_handler),
                        sysmem_handler = std::move(sysmem_handler),
                        tee_handler = std::move(tee_handler)](
                           component::OutgoingDirectory* outgoing) mutable {
      ZX_ASSERT(outgoing->Serve(std::move(pdev_server)).is_ok());
      ZX_ASSERT(outgoing->Serve(std::move(sysmem_server)).is_ok());
      ZX_ASSERT(outgoing->Serve(std::move(tee_server)).is_ok());

      ZX_ASSERT(
          outgoing->AddService<fuchsia_hardware_platform_device::Service>(std::move(pdev_handler))
              .is_ok());
      ZX_ASSERT(outgoing->AddService<fuchsia_hardware_sysmem::Service>(std::move(sysmem_handler))
                    .is_ok());
      ZX_ASSERT(
          outgoing->AddService<fuchsia_hardware_tee::Service>(std::move(tee_handler)).is_ok());
    });

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
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  // TODO(fxb/124464): Migrate test to use dispatcher integration.
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED();
  async_patterns::TestDispatcherBound<fake_pdev::FakePDevFidl> pdev_{incoming_loop_.dispatcher(),
                                                                     std::in_place};
  async_patterns::TestDispatcherBound<FakeSysmem> sysmem_{incoming_loop_.dispatcher(),
                                                          std::in_place};
  async_patterns::TestDispatcherBound<FakeTee> tee_{incoming_loop_.dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<component::OutgoingDirectory> outgoing_{
      incoming_loop_.dispatcher(), std::in_place, async_patterns::PassDispatcher};
  amlogic_secure_mem::AmlogicSecureMemDevice* dev_;
  fdf::Dispatcher dispatcher_;

  libsync::Completion shutdown_completion_;
};

TEST_F(AmlogicSecureMemTest, GetSecureMemoryPhysicalAddressBadVmo) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ZX_PAGE_SIZE, 0, &vmo));

  ASSERT_TRUE(dev()->GetSecureMemoryPhysicalAddress(std::move(vmo)).is_error());
}
