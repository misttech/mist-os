// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../optee-controller.h"

#include <fidl/fuchsia.hardware.rpmb/cpp/wire.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fake-bti/bti.h>
#include <lib/fake-object/object.h>
#include <lib/fake-resource/resource.h>
#include <lib/fdf/env.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/sync/completion.h>
#include <lib/zx/bti.h>
#include <stdlib.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <functional>

#include <ddktl/suspend-txn.h>
#include <zxtest/zxtest.h>

#include "../optee-smc.h"
#include "../tee-smc.h"
#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

struct SharedMemoryInfo {
  zx_paddr_t address = 0;
  size_t size = 0;
};

// This will be populated once the FakePdev creates the fake contiguous vmo so we can use the
// physical addresses within it.
static SharedMemoryInfo gSharedMemory = {};

constexpr fuchsia_tee::wire::Uuid kOpteeOsUuid = {
    0x486178E0, 0xE7F8, 0x11E3, {0xBC, 0x5E, 0x00, 0x02, 0xA5, 0xD5, 0xC5, 0x1B}};

using SmcCb = std::function<void(const zx_smc_parameters_t*, zx_smc_result_t*)>;
static SmcCb call_with_arg_handler;
static uint32_t call_with_args_count = 0;
static std::mutex handler_mut;

void SetSmcCallWithArgHandler(SmcCb handler) {
  std::lock_guard<std::mutex> lock(handler_mut);
  call_with_arg_handler = std::move(handler);
}

zx_status_t zx_smc_call(zx_handle_t handle, const zx_smc_parameters_t* parameters,
                        zx_smc_result_t* out_smc_result) {
  EXPECT_TRUE(parameters);
  EXPECT_TRUE(out_smc_result);
  switch (parameters->func_id) {
    case tee_smc::kTrustedOsCallUidFuncId:
      out_smc_result->arg0 = optee::kOpteeApiUid_0;
      out_smc_result->arg1 = optee::kOpteeApiUid_1;
      out_smc_result->arg2 = optee::kOpteeApiUid_2;
      out_smc_result->arg3 = optee::kOpteeApiUid_3;
      break;
    case tee_smc::kTrustedOsCallRevisionFuncId:
      out_smc_result->arg0 = optee::kOpteeApiRevisionMajor;
      out_smc_result->arg1 = optee::kOpteeApiRevisionMinor;
      break;
    case optee::kGetOsRevisionFuncId:
      out_smc_result->arg0 = 1;
      out_smc_result->arg1 = 0;
      break;
    case optee::kExchangeCapabilitiesFuncId:
      out_smc_result->arg0 = optee::kReturnOk;
      out_smc_result->arg1 =
          optee::kSecureCapHasReservedSharedMem | optee::kSecureCapCanUsePrevUnregisteredSharedMem;
      break;
    case optee::kGetSharedMemConfigFuncId:
      out_smc_result->arg0 = optee::kReturnOk;
      out_smc_result->arg1 = gSharedMemory.address;
      out_smc_result->arg2 = gSharedMemory.size;
      break;
    case optee::kCallWithArgFuncId: {
      call_with_args_count++;
      SmcCb handler;
      {
        std::lock_guard<std::mutex> lock(handler_mut);
        std::swap(handler, call_with_arg_handler);
      }
      if (handler != nullptr) {
        handler(parameters, out_smc_result);
      } else {
        out_smc_result->arg0 = optee::kReturnOk;
      }
    } break;
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
  return ZX_OK;
}

namespace optee {
namespace {

class FakePDev : public ddk::PDevProtocol<FakePDev, ddk::base_protocol> {
 public:
  FakePDev() {}

  const pdev_protocol_ops_t* proto_ops() const { return &pdev_protocol_ops_; }

  zx_status_t PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio) {
    EXPECT_EQ(index, 0);
    constexpr size_t kSecureWorldMemorySize = 0x20000;

    EXPECT_OK(zx::vmo::create_contiguous(*fake_bti_, 0x20000, 0, &fake_vmo_));

    // Briefly pin the vmo to get the paddr for populating the gSharedMemory object
    zx_paddr_t secure_world_paddr;
    zx::pmt pmt;
    EXPECT_OK(fake_bti_->pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, fake_vmo_, 0,
                             kSecureWorldMemorySize, &secure_world_paddr, 1, &pmt));
    // Use the second half of the secure world range to use as shared memory
    gSharedMemory.address = secure_world_paddr + (kSecureWorldMemorySize / 2);
    gSharedMemory.size = kSecureWorldMemorySize / 2;
    EXPECT_OK(pmt.unpin());

    out_mmio->vmo = fake_vmo_.get();
    out_mmio->offset = 0;
    out_mmio->size = kSecureWorldMemorySize;
    return ZX_OK;
  }

  zx_status_t PDevGetBti(uint32_t index, zx::bti* out_bti) {
    zx_status_t status = fake_bti_create(out_bti->reset_and_get_address());
    // Stash an unowned copy of it, for the purposes of creating a contiguous vmo to back the secure
    // world memory
    fake_bti_ = out_bti->borrow();
    return status;
  }

  zx_status_t PDevGetSmc(uint32_t index, zx::resource* out_resource) {
    // Just use a fake root resource for now, which is technically eligible for SMC calls. A more
    // appropriate object would be to use the root resource to mint an SMC resource type.
    return fake_root_resource_create(out_resource->reset_and_get_address());
  }

  zx_status_t PDevGetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t PDevGetDeviceInfo(pdev_device_info_t* out_info) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t PDevGetBoardInfo(pdev_board_info_t* out_info) { return ZX_ERR_NOT_SUPPORTED; }

 private:
  zx::unowned_bti fake_bti_;
  zx::vmo fake_vmo_;
};

class FakeSysmem : public ddk::SysmemProtocol<FakeSysmem> {
 public:
  FakeSysmem() {}

  const sysmem_protocol_ops_t* proto_ops() const { return &sysmem_protocol_ops_; }

  zx_status_t SysmemConnect(zx::channel allocator2_request) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t SysmemRegisterHeap(uint64_t heap, zx::channel heap_connection) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t SysmemRegisterSecureMem(zx::channel tee_connection) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t SysmemUnregisterSecureMem() { return ZX_ERR_NOT_SUPPORTED; }
};

class FakeRpmbService {
 public:
  FakeRpmbService() : outgoing_(loop_.dispatcher()) {}
  ~FakeRpmbService() { loop_.Shutdown(); }

  fidl::ClientEnd<fuchsia_io::Directory> Connect() {
    auto device_handler = [](fidl::ServerEnd<fuchsia_hardware_rpmb::Rpmb> request) {};
    fuchsia_hardware_rpmb::Service::InstanceHandler handler({.device = std::move(device_handler)});

    auto service_result = outgoing_.AddService<fuchsia_hardware_rpmb::Service>(std::move(handler));
    ZX_ASSERT(service_result.is_ok());

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    ZX_ASSERT(outgoing_.Serve(std::move(endpoints->server)).is_ok());

    return std::move(endpoints->client);
  }

 private:
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  component::OutgoingDirectory outgoing_;
};

class FakeTeeService : public fidl::WireServer<fuchsia_hardware_tee::DeviceConnector> {
 public:
  explicit FakeTeeService(OpteeController* optee)
      : dispatcher_(fdf::Dispatcher::GetCurrent()),
        outgoing_(dispatcher_->async_dispatcher()),
        optee_(optee) {}

  fidl::ClientEnd<fuchsia_io::Directory> Connect() {
    auto device_handler = [this](fidl::ServerEnd<fuchsia_hardware_tee::DeviceConnector> request) {
      fidl::BindServer(dispatcher_->async_dispatcher(), std::move(request), this);
      client_connected_.Signal();
    };
    fuchsia_hardware_tee::Service::InstanceHandler handler(
        {.device_connector = std::move(device_handler)});

    auto service_result = outgoing_.AddService<fuchsia_hardware_tee::Service>(std::move(handler));
    ZX_ASSERT(service_result.is_ok());

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    ZX_ASSERT(outgoing_.Serve(std::move(endpoints->server)).is_ok());

    return std::move(endpoints->client);
  }

  void ConnectToApplication(ConnectToApplicationRequestView request,
                            ConnectToApplicationCompleter::Sync& completer) override {
    optee_->ConnectToApplication(std::move(request), completer);
  }

  void ConnectToDeviceInfo(ConnectToDeviceInfoRequestView request,
                           ConnectToDeviceInfoCompleter::Sync& completer) override {}

  void WaitForClientConnected() { ZX_ASSERT(fdf::WaitFor(client_connected_).is_ok()); }

 private:
  fdf::UnownedDispatcher dispatcher_;
  component::OutgoingDirectory outgoing_;
  OpteeController* optee_;

  libsync::Completion client_connected_;
};

class FakeDdkOptee : public zxtest::Test {
 public:
  FakeDdkOptee() : clients_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

  void SetUp() override {
    ASSERT_OK(clients_loop_.StartThread());
    ASSERT_OK(clients_loop_.StartThread());
    ASSERT_OK(clients_loop_.StartThread());
    parent_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto_ops(), &pdev_, "pdev");
    parent_->AddProtocol(ZX_PROTOCOL_SYSMEM, sysmem_.proto_ops(), &sysmem_, "sysmem");
    parent_->AddFidlService(fuchsia_hardware_rpmb::Service::Name, rpmb_service_.Connect(), "rpmb");

    ASSERT_OK(OpteeController::Create(nullptr, parent_.get()));
    optee_ = parent_->GetLatestChild()->GetDeviceContext<OpteeController>();

    tee_service_ = FakeTeeService(optee_);
    parent_->AddFidlService(fuchsia_hardware_tee::Service::Name, tee_service_->Connect(), "tee");

    zx::result client_end =
        optee_->DdkConnectFragmentFidlProtocol<fuchsia_hardware_tee::Service::DeviceConnector>(
            "tee");
    ASSERT_OK(client_end.status_value());
    tee_proto_client_.Bind(std::move(client_end.value()));

    tee_service_->WaitForClientConnected();

    call_with_args_count = 0;
  }

  void TearDown() override {
    device_async_remove(parent_->GetLatestChild());
    EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(parent_.get()));
  }

 protected:
  FakePDev pdev_;
  FakeSysmem sysmem_;
  FakeRpmbService rpmb_service_;

  fdf_testing::DriverRuntimeEnv runtime_;
  fdf::TestSynchronizedDispatcher driver_dispatcher_{fdf::kDispatcherDefault};

  // TODO(fxb/124464): Migrate test to use dispatcher integration.
  std::shared_ptr<MockDevice> parent_ =
      MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED();
  OpteeController* optee_ = nullptr;

  async::Loop clients_loop_;
  std::optional<FakeTeeService> tee_service_;
  fidl::WireSyncClient<fuchsia_hardware_tee::DeviceConnector> tee_proto_client_;
};

TEST_F(FakeDdkOptee, PmtUnpinned) {
  zx_handle_t pmt_handle = optee_->pmt().get();
  EXPECT_NE(pmt_handle, ZX_HANDLE_INVALID);

  EXPECT_TRUE(fake_object::FakeHandleTable().Get(pmt_handle).is_ok());
  EXPECT_EQ(ZX_OBJ_TYPE_PMT, fake_object::FakeHandleTable().Get(pmt_handle)->type());

  optee_->zxdev()->SuspendNewOp(DEV_POWER_STATE_D3COLD, false, DEVICE_SUSPEND_REASON_REBOOT);
  EXPECT_FALSE(fake_object::FakeHandleTable().Get(pmt_handle).is_ok());
}

TEST_F(FakeDdkOptee, RpmbTest) { EXPECT_EQ(optee_->RpmbConnectServer().status_value(), ZX_OK); }

TEST_F(FakeDdkOptee, MultiThreadTest) {
  fidl::ClientEnd<fuchsia_tee::Application> tee_app_client[2];
  libsync::Completion completion1;
  libsync::Completion completion2;
  libsync::Completion smc_completion;
  libsync::Completion smc_completion1;
  zx_status_t status;

  for (auto& i : tee_app_client) {
    auto tee_endpoints = fidl::CreateEndpoints<fuchsia_tee::Application>();
    ASSERT_OK(tee_endpoints.status_value());

    i = std::move(tee_endpoints->client);

    auto result = tee_proto_client_->ConnectToApplication(
        kOpteeOsUuid, fidl::ClientEnd<::fuchsia_tee_manager::Provider>(),
        std::move(tee_endpoints->server));
    ASSERT_OK(result.status());
  }

  fidl::WireSharedClient fidl_client1(std::move(tee_app_client[0]), clients_loop_.dispatcher());
  fidl::WireSharedClient fidl_client2(std::move(tee_app_client[1]), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      sync_completion_signal(smc_completion1.get());
      sync_completion_wait(smc_completion.get(), ZX_TIME_INFINITE);
      out->arg0 = optee::kReturnOk;
    });
  }
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion1.get());
            });
  }
  status = sync_completion_wait(completion1.get(), ZX_SEC(1));
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);
  ASSERT_OK(fdf::WaitFor(smc_completion1));

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      out->arg0 = optee::kReturnOk;
    });
  }
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client2->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion2.get());
            });
  }
  ASSERT_OK(fdf::WaitFor(completion2));
  sync_completion_signal(smc_completion.get());
  ASSERT_OK(fdf::WaitFor(completion1));
  EXPECT_EQ(call_with_args_count, 2);
}

TEST_F(FakeDdkOptee, TheadLimitCorrectOrder) {
  fidl::ClientEnd<fuchsia_tee::Application> tee_app_client[2];
  libsync::Completion completion1;
  libsync::Completion completion2;
  libsync::Completion smc_completion;
  zx_status_t status;

  for (auto& i : tee_app_client) {
    auto tee_endpoints = fidl::CreateEndpoints<fuchsia_tee::Application>();
    ASSERT_OK(tee_endpoints.status_value());

    i = std::move(tee_endpoints->client);

    auto result = tee_proto_client_->ConnectToApplication(
        kOpteeOsUuid, fidl::ClientEnd<::fuchsia_tee_manager::Provider>(),
        std::move(tee_endpoints->server));
    ASSERT_OK(result.status());
  }

  fidl::WireSharedClient fidl_client1(std::move(tee_app_client[0]), clients_loop_.dispatcher());
  fidl::WireSharedClient fidl_client2(std::move(tee_app_client[1]), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      sync_completion_signal(smc_completion.get());
      out->arg0 = optee::kReturnEThreadLimit;
    });
  }
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion1.get());
            });
  }

  ASSERT_OK(fdf::WaitFor(smc_completion));
  status = sync_completion_wait(completion1.get(), ZX_SEC(1));
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);
  EXPECT_EQ(call_with_args_count, 1);
  EXPECT_EQ(optee_->CommandQueueSize(), 1);

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      out->arg0 = optee::kReturnOk;
    });
  }
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client2->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion2.get());
            });
  }

  ASSERT_OK(fdf::WaitFor(completion2));
  ASSERT_OK(fdf::WaitFor(completion1));
  EXPECT_EQ(call_with_args_count, 3);
  EXPECT_EQ(optee_->CommandQueueSize(), 0);
  EXPECT_EQ(optee_->CommandQueueWaitSize(), 0);
}

TEST_F(FakeDdkOptee, TheadLimitWrongOrder) {
  fidl::ClientEnd<fuchsia_tee::Application> tee_app_client[3];
  libsync::Completion completion1;
  libsync::Completion completion2;
  libsync::Completion completion3;
  libsync::Completion smc_completion;
  libsync::Completion smc_sleep_completion;

  for (auto& i : tee_app_client) {
    auto tee_endpoints = fidl::CreateEndpoints<fuchsia_tee::Application>();
    ASSERT_OK(tee_endpoints.status_value());

    i = std::move(tee_endpoints->client);

    auto result = tee_proto_client_->ConnectToApplication(
        kOpteeOsUuid, fidl::ClientEnd<::fuchsia_tee_manager::Provider>(),
        std::move(tee_endpoints->server));
    ASSERT_OK(result.status());
  }

  fidl::WireSharedClient fidl_client1(std::move(tee_app_client[0]), clients_loop_.dispatcher());
  fidl::WireSharedClient fidl_client2(std::move(tee_app_client[1]), clients_loop_.dispatcher());
  fidl::WireSharedClient fidl_client3(std::move(tee_app_client[2]), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      sync_completion_signal(smc_completion.get());
      sync_completion_wait(smc_sleep_completion.get(), ZX_TIME_INFINITE);
      out->arg0 = optee::kReturnOk;
    });
  }
  {  // first client is just sleeping for a long time (without ThreadLimit)
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion1.get());
            });
  }

  ASSERT_OK(fdf::WaitFor(smc_completion));
  EXPECT_FALSE(sync_completion_signaled(completion1.get()));
  EXPECT_EQ(call_with_args_count, 1);
  sync_completion_reset(smc_completion.get());

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      sync_completion_signal(smc_completion.get());
      out->arg0 = optee::kReturnEThreadLimit;
    });
  }
  {  // 2nd client got ThreadLimit
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client2->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion2.get());
            });
  }

  ASSERT_OK(fdf::WaitFor(smc_completion));
  EXPECT_FALSE(sync_completion_signaled(completion2.get()));
  EXPECT_EQ(call_with_args_count, 2);
  EXPECT_EQ(optee_->CommandQueueSize(), 2);

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      out->arg0 = optee::kReturnOk;
    });
  }
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client3->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion3.get());
            });
  }

  ASSERT_OK(fdf::WaitFor(completion3));
  ASSERT_OK(fdf::WaitFor(completion2));
  EXPECT_EQ(call_with_args_count, 4);
  sync_completion_signal(smc_sleep_completion.get());
  ASSERT_OK(fdf::WaitFor(completion1));
  EXPECT_EQ(optee_->CommandQueueSize(), 0);
  EXPECT_EQ(optee_->CommandQueueWaitSize(), 0);
}

TEST_F(FakeDdkOptee, TheadLimitWrongOrderCascade) {
  fidl::ClientEnd<fuchsia_tee::Application> tee_app_client[3];
  libsync::Completion completion1;
  libsync::Completion completion2;
  libsync::Completion completion3;
  libsync::Completion smc_completion;
  libsync::Completion smc_sleep_completion1;
  libsync::Completion smc_sleep_completion2;

  for (auto& i : tee_app_client) {
    auto tee_endpoints = fidl::CreateEndpoints<fuchsia_tee::Application>();
    ASSERT_OK(tee_endpoints.status_value());

    i = std::move(tee_endpoints->client);

    auto result = tee_proto_client_->ConnectToApplication(
        kOpteeOsUuid, fidl::ClientEnd<::fuchsia_tee_manager::Provider>(),
        std::move(tee_endpoints->server));
    ASSERT_OK(result.status());
  }

  fidl::WireSharedClient fidl_client1(std::move(tee_app_client[0]), clients_loop_.dispatcher());
  fidl::WireSharedClient fidl_client2(std::move(tee_app_client[1]), clients_loop_.dispatcher());
  fidl::WireSharedClient fidl_client3(std::move(tee_app_client[2]), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      sync_completion_signal(smc_completion.get());
      sync_completion_wait(smc_sleep_completion1.get(), ZX_TIME_INFINITE);
      out->arg0 = optee::kReturnEThreadLimit;
    });
  }
  {  // first client is just sleeping for a long time (without ThreadLimit)
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client1->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion1.get());
            });
  }

  ASSERT_OK(fdf::WaitFor(smc_completion));
  EXPECT_FALSE(sync_completion_signaled(completion1.get()));
  EXPECT_EQ(call_with_args_count, 1);
  sync_completion_reset(smc_completion.get());

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      sync_completion_signal(smc_completion.get());
      sync_completion_wait(smc_sleep_completion2.get(), ZX_TIME_INFINITE);
      out->arg0 = optee::kReturnOk;
    });
  }
  {  // 2nd client got ThreadLimit
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client2->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion2.get());
            });
  }

  ASSERT_OK(fdf::WaitFor(smc_completion));
  EXPECT_FALSE(sync_completion_signaled(completion2.get()));
  EXPECT_EQ(call_with_args_count, 2);
  EXPECT_EQ(optee_->CommandQueueSize(), 2);

  {
    SetSmcCallWithArgHandler([&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
      out->arg0 = optee::kReturnOk;
    });
  }
  {
    fidl::VectorView<fuchsia_tee::wire::Parameter> parameter_set;
    fidl_client3->OpenSession2(parameter_set)
        .ThenExactlyOnce(
            [&](::fidl::WireUnownedResult<::fuchsia_tee::Application::OpenSession2>& result) {
              if (!result.ok()) {
                FAIL("OpenSession2 failed: %s", result.error().FormatDescription().c_str());
                return;
              }
              sync_completion_signal(completion3.get());
            });
  }
  ASSERT_OK(fdf::WaitFor(completion3));
  EXPECT_EQ(call_with_args_count, 3);

  sync_completion_signal(smc_sleep_completion2.get());
  ASSERT_OK(fdf::WaitFor(completion2));
  EXPECT_EQ(call_with_args_count, 3);
  sync_completion_signal(smc_sleep_completion1.get());
  ASSERT_OK(fdf::WaitFor(completion1));
  EXPECT_EQ(call_with_args_count, 4);

  EXPECT_EQ(optee_->CommandQueueSize(), 0);
  EXPECT_EQ(optee_->CommandQueueWaitSize(), 0);
}

}  // namespace
}  // namespace optee
