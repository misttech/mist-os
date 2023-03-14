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
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/sync/completion.h>
#include <lib/zx/bti.h>
#include <lib/zx/object.h>
#include <lib/zx/resource.h>
#include <stdlib.h>
#include <zircon/syscalls/resource.h>
#include <zircon/syscalls/smc.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <functional>

#include <ddktl/suspend-txn.h>
#include <zxtest/zxtest.h>

#include "../optee-smc.h"
#include "../tee-smc.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

struct SharedMemoryInfo {
  zx_paddr_t address = 0;
  size_t size = 0;
};

// This will be populated once the FakePdev creates the fake contiguous vmo so we can use the
// physical addresses within it.
namespace {
SharedMemoryInfo gSharedMemory = {};
// Results like shared memory values can change depending on how PDev is
// interacted with, so this method can be called any time a source is modified.
void UpdateSmcResults(const zx::unowned_resource& smc) {
  std::vector<std::pair<zx_smc_parameters_t, zx_smc_result_t>> smc_results;
  smc_results.emplace_back(tee_smc::CreateSmcFunctionCall(tee_smc::kTrustedOsCallUidFuncId),
                           zx_smc_result_t{.arg0 = optee::kOpteeApiUid_0,
                                           .arg1 = optee::kOpteeApiUid_1,
                                           .arg2 = optee::kOpteeApiUid_2,
                                           .arg3 = optee::kOpteeApiUid_3});
  smc_results.emplace_back(tee_smc::CreateSmcFunctionCall(tee_smc::kTrustedOsCallRevisionFuncId),
                           zx_smc_result_t{.arg0 = optee::kOpteeApiRevisionMajor,
                                           .arg1 = optee::kOpteeApiRevisionMinor});

  smc_results.emplace_back(tee_smc::CreateSmcFunctionCall(optee::kGetOsRevisionFuncId),
                           zx_smc_result_t{.arg0 = 1, .arg1 = 0});
  smc_results.emplace_back(tee_smc::CreateSmcFunctionCall(optee::kExchangeCapabilitiesFuncId),
                           zx_smc_result{.arg0 = optee::kReturnOk,
                                         .arg1 = optee::kSecureCapHasReservedSharedMem |
                                                 optee::kSecureCapCanUsePrevUnregisteredSharedMem});
  smc_results.emplace_back(
      tee_smc::CreateSmcFunctionCall(optee::kGetSharedMemConfigFuncId),
      zx_smc_result{
          .arg0 = optee::kReturnOk, .arg1 = gSharedMemory.address, .arg2 = gSharedMemory.size});
  smc_results.emplace_back(tee_smc::CreateSmcFunctionCall(optee::kCallWithArgFuncId),
                           zx_smc_result{.arg0 = optee::kReturnOk});
  fake_smc_set_results(smc, smc_results);
}
}  // namespace

constexpr uuid_t kOpteeOsUuid = {
    0x486178E0, 0xE7F8, 0x11E3, {0xBC, 0x5E, 0x00, 0x02, 0xA5, 0xD5, 0xC5, 0x1B}};

using SmcCb = std::function<void(const zx_smc_parameters_t*, zx_smc_result_t*)>;
void SetSmcCallWithArgHandler(const zx::unowned_resource& smc, const SmcCb& handler) {
  fake_smc_set_handler(smc, handler);
}

namespace optee {
namespace {

class FakePDev : public ddk::PDevProtocol<FakePDev, ddk::base_protocol> {
 public:
  FakePDev() { ASSERT_OK(fake_root_resource_create(fake_root_.reset_and_get_address())); }

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
    UpdateSmcResults(fake_smc_);
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
    zx_status_t status = zx::resource::create(fake_root_, ZX_RSRC_KIND_SMC, /*base=*/0, /*len=*/0,
                                              /*name=*/nullptr, /*namelen=*/0, out_resource);
    if (status == ZX_OK) {
      fake_smc_ = out_resource->borrow();
      UpdateSmcResults(fake_smc_);
    }
    return status;
  }

  zx_status_t PDevGetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t PDevGetDeviceInfo(pdev_device_info_t* out_info) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t PDevGetBoardInfo(pdev_board_info_t* out_info) { return ZX_ERR_NOT_SUPPORTED; }

  const zx::unowned_resource& fake_smc() { return fake_smc_; }

 private:
  zx::unowned_bti fake_bti_;
  zx::unowned_resource fake_smc_;
  zx::resource fake_root_;
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

class FakeDdkOptee : public zxtest::Test {
 public:
  FakeDdkOptee() : clients_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    ASSERT_OK(clients_loop_.StartThread());
    ASSERT_OK(clients_loop_.StartThread());
    ASSERT_OK(clients_loop_.StartThread());
    parent_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto_ops(), &pdev_, "pdev");
    parent_->AddProtocol(ZX_PROTOCOL_SYSMEM, sysmem_.proto_ops(), &sysmem_, "sysmem");
    parent_->AddFidlService(fuchsia_hardware_rpmb::Service::Name, rpmb_service_.Connect(), "rpmb");

    ASSERT_OK(OpteeController::Create(nullptr, parent_.get()));
    optee_ = parent_->GetLatestChild()->GetDeviceContext<OpteeController>();
  }

 protected:
  FakePDev pdev_;
  FakeSysmem sysmem_;
  FakeRpmbService rpmb_service_;

  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();
  OpteeController* optee_ = nullptr;

  async::Loop clients_loop_;
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
  zx::channel tee_app_client[2];
  sync_completion_t completion1;
  sync_completion_t completion2;
  sync_completion_t smc_completion;
  sync_completion_t smc_completion1;
  zx_status_t status;

  for (auto& i : tee_app_client) {
    zx::channel tee_app_server;
    ASSERT_OK(zx::channel::create(0, &i, &tee_app_server));
    zx::channel provider_server;
    zx::channel provider_client;
    ASSERT_OK(zx::channel::create(0, &provider_client, &provider_server));

    optee_->TeeConnectToApplication(&kOpteeOsUuid, std::move(tee_app_server),
                                    std::move(provider_client));
  }

  auto client_end1 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[0]));
  fidl::WireSharedClient fidl_client1(std::move(client_end1), clients_loop_.dispatcher());
  auto client_end2 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[1]));
  fidl::WireSharedClient fidl_client2(std::move(client_end2), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
                               sync_completion_signal(&smc_completion1);
                               sync_completion_wait(&smc_completion, ZX_TIME_INFINITE);
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
              sync_completion_signal(&completion1);
            });
  }
  status = sync_completion_wait(&completion1, ZX_SEC(1));
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);
  sync_completion_wait(&smc_completion1, ZX_TIME_INFINITE);

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
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
              sync_completion_signal(&completion2);
            });
  }
  sync_completion_wait(&completion2, ZX_TIME_INFINITE);
  sync_completion_signal(&smc_completion);
  sync_completion_wait(&completion1, ZX_TIME_INFINITE);
}

TEST_F(FakeDdkOptee, TheadLimitCorrectOrder) {
  zx::channel tee_app_client[2];
  sync_completion_t completion1;
  sync_completion_t completion2;
  sync_completion_t smc_completion;
  zx_status_t status;

  for (auto& i : tee_app_client) {
    zx::channel tee_app_server;
    ASSERT_OK(zx::channel::create(0, &i, &tee_app_server));
    zx::channel provider_server;
    zx::channel provider_client;
    ASSERT_OK(zx::channel::create(0, &provider_client, &provider_server));

    optee_->TeeConnectToApplication(&kOpteeOsUuid, std::move(tee_app_server),
                                    std::move(provider_client));
  }

  auto client_end1 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[0]));
  fidl::WireSharedClient fidl_client1(std::move(client_end1), clients_loop_.dispatcher());
  auto client_end2 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[1]));
  fidl::WireSharedClient fidl_client2(std::move(client_end2), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
                               sync_completion_signal(&smc_completion);
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
              sync_completion_signal(&completion1);
            });
  }

  sync_completion_wait(&smc_completion, ZX_TIME_INFINITE);
  status = sync_completion_wait(&completion1, ZX_SEC(1));
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);
  EXPECT_EQ(optee_->CommandQueueSize(), 1);

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
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
              sync_completion_signal(&completion2);
            });
  }

  sync_completion_wait(&completion2, ZX_TIME_INFINITE);
  sync_completion_wait(&completion1, ZX_TIME_INFINITE);
  EXPECT_EQ(optee_->CommandQueueSize(), 0);
  EXPECT_EQ(optee_->CommandQueueWaitSize(), 0);
}

TEST_F(FakeDdkOptee, TheadLimitWrongOrder) {
  zx::channel tee_app_client[3];
  sync_completion_t completion1;
  sync_completion_t completion2;
  sync_completion_t completion3;
  sync_completion_t smc_completion;
  sync_completion_t smc_sleep_completion;

  for (auto& i : tee_app_client) {
    zx::channel tee_app_server;
    ASSERT_OK(zx::channel::create(0, &i, &tee_app_server));
    zx::channel provider_server;
    zx::channel provider_client;
    ASSERT_OK(zx::channel::create(0, &provider_client, &provider_server));

    optee_->TeeConnectToApplication(&kOpteeOsUuid, std::move(tee_app_server),
                                    std::move(provider_client));
  }

  auto client_end1 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[0]));
  fidl::WireSharedClient fidl_client1(std::move(client_end1), clients_loop_.dispatcher());
  auto client_end2 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[1]));
  fidl::WireSharedClient fidl_client2(std::move(client_end2), clients_loop_.dispatcher());
  auto client_end3 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[2]));
  fidl::WireSharedClient fidl_client3(std::move(client_end3), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
                               sync_completion_signal(&smc_completion);
                               sync_completion_wait(&smc_sleep_completion, ZX_TIME_INFINITE);
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
              sync_completion_signal(&completion1);
            });
  }

  sync_completion_wait(&smc_completion, ZX_TIME_INFINITE);
  EXPECT_FALSE(sync_completion_signaled(&completion1));
  sync_completion_reset(&smc_completion);

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
                               sync_completion_signal(&smc_completion);
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
              sync_completion_signal(&completion2);
            });
  }

  sync_completion_wait(&smc_completion, ZX_TIME_INFINITE);
  EXPECT_FALSE(sync_completion_signaled(&completion2));
  EXPECT_EQ(optee_->CommandQueueSize(), 2);

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
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
              sync_completion_signal(&completion3);
            });
  }

  sync_completion_wait(&completion3, ZX_TIME_INFINITE);
  sync_completion_wait(&completion2, ZX_TIME_INFINITE);
  sync_completion_signal(&smc_sleep_completion);
  sync_completion_wait(&completion1, ZX_TIME_INFINITE);
  EXPECT_EQ(optee_->CommandQueueSize(), 0);
  EXPECT_EQ(optee_->CommandQueueWaitSize(), 0);
}

TEST_F(FakeDdkOptee, TheadLimitWrongOrderCascade) {
  zx::channel tee_app_client[3];
  sync_completion_t completion1;
  sync_completion_t completion2;
  sync_completion_t completion3;
  sync_completion_t smc_completion;
  sync_completion_t smc_sleep_completion1;
  sync_completion_t smc_sleep_completion2;

  for (auto& i : tee_app_client) {
    zx::channel tee_app_server;
    ASSERT_OK(zx::channel::create(0, &i, &tee_app_server));
    zx::channel provider_server;
    zx::channel provider_client;
    ASSERT_OK(zx::channel::create(0, &provider_client, &provider_server));

    optee_->TeeConnectToApplication(&kOpteeOsUuid, std::move(tee_app_server),
                                    std::move(provider_client));
  }

  auto client_end1 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[0]));
  fidl::WireSharedClient fidl_client1(std::move(client_end1), clients_loop_.dispatcher());
  auto client_end2 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[1]));
  fidl::WireSharedClient fidl_client2(std::move(client_end2), clients_loop_.dispatcher());
  auto client_end3 = fidl::ClientEnd<fuchsia_tee::Application>(std::move(tee_app_client[2]));
  fidl::WireSharedClient fidl_client3(std::move(client_end3), clients_loop_.dispatcher());

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
                               sync_completion_signal(&smc_completion);
                               sync_completion_wait(&smc_sleep_completion1, ZX_TIME_INFINITE);
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
              sync_completion_signal(&completion1);
            });
  }

  sync_completion_wait(&smc_completion, ZX_TIME_INFINITE);
  EXPECT_FALSE(sync_completion_signaled(&completion1));
  sync_completion_reset(&smc_completion);

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
                               sync_completion_signal(&smc_completion);
                               sync_completion_wait(&smc_sleep_completion2, ZX_TIME_INFINITE);
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
              sync_completion_signal(&completion2);
            });
  }

  sync_completion_wait(&smc_completion, ZX_TIME_INFINITE);
  EXPECT_FALSE(sync_completion_signaled(&completion2));
  EXPECT_EQ(optee_->CommandQueueSize(), 2);

  {
    SetSmcCallWithArgHandler(pdev_.fake_smc(),
                             [&](const zx_smc_parameters_t* params, zx_smc_result_t* out) {
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
              sync_completion_signal(&completion3);
            });
  }
  sync_completion_wait(&completion3, ZX_TIME_INFINITE);

  sync_completion_signal(&smc_sleep_completion2);
  sync_completion_wait(&completion2, ZX_TIME_INFINITE);
  sync_completion_signal(&smc_sleep_completion1);
  sync_completion_wait(&completion1, ZX_TIME_INFINITE);

  EXPECT_EQ(optee_->CommandQueueSize(), 0);
  EXPECT_EQ(optee_->CommandQueueWaitSize(), 0);
}

}  // namespace
}  // namespace optee
