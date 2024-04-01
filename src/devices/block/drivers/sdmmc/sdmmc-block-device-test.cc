// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-block-device.h"

#include <endian.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>
#include <lib/sdmmc/hw.h>
#include <zircon/errors.h>

#include <memory>

#include <fbl/algorithm.h>
#include <zxtest/zxtest.h>

#include "fake-sdmmc-device.h"
#include "sdmmc-partition-device.h"
#include "sdmmc-root-device.h"
#include "sdmmc-rpmb-device.h"
#include "sdmmc-types.h"

namespace sdmmc {

class TestSdmmcRootDevice : public SdmmcRootDevice {
 public:
  // Modify these static variables to configure the behaviour of this test device.
  static bool use_fidl_;
  static bool is_sd_;
  static FakeSdmmcDevice sdmmc_;

  TestSdmmcRootDevice(fdf::DriverStartArgs start_args,
                      fdf::UnownedSynchronizedDispatcher dispatcher)
      : SdmmcRootDevice(std::move(start_args), std::move(dispatcher)) {}

 protected:
  zx_status_t Init(
      fidl::ObjectView<fuchsia_hardware_sdmmc::wire::SdmmcMetadata> metadata) override {
    std::unique_ptr<SdmmcDevice> sdmmc;
    if (use_fidl_) {
      zx::result client_end = sdmmc_.GetFidlClientEnd();
      if (client_end.is_error()) {
        return client_end.error_value();
      }
      sdmmc = std::make_unique<SdmmcDevice>(this, std::move(*client_end));
    } else {
      sdmmc = std::make_unique<SdmmcDevice>(this, sdmmc_.GetClient());
    }

    zx_status_t status;
    if (status = sdmmc->RefreshHostInfo(); status != ZX_OK) {
      return status;
    }
    if (status = sdmmc->HwReset(); status != ZX_OK) {
      return status;
    }

    std::unique_ptr<SdmmcBlockDevice> block_device;
    if (status = SdmmcBlockDevice::Create(this, std::move(sdmmc), &block_device); status != ZX_OK) {
      return status;
    }
    if (status = is_sd_ ? block_device->ProbeSd(*metadata) : block_device->ProbeMmc(*metadata);
        status != ZX_OK) {
      return status;
    }
    if (status = block_device->AddDevice(); status != ZX_OK) {
      return status;
    }
    child_device_ = std::move(block_device);
    return ZX_OK;
  }
};

bool TestSdmmcRootDevice::use_fidl_;
bool TestSdmmcRootDevice::is_sd_;
FakeSdmmcDevice TestSdmmcRootDevice::sdmmc_;

struct IncomingNamespace {
  fdf_testing::TestNode node{"root"};
  fdf_testing::TestEnvironment env{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer device_server;
};

class SdmmcBlockDeviceTest : public zxtest::TestWithParam<bool> {
 public:
  SdmmcBlockDeviceTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher_->async_dispatcher(), std::in_place),
        loop_(&kAsyncLoopConfigAttachToCurrentThread) {}

  void SetUp() override {
    sdmmc_.Reset();

    sdmmc_.set_command_callback(SDMMC_SEND_CSD, [](uint32_t out_response[4]) -> void {
      uint8_t* response = reinterpret_cast<uint8_t*>(out_response);
      response[MMC_CSD_SPEC_VERSION] = MMC_CID_SPEC_VRSN_40 << 2;
      response[MMC_CSD_SIZE_START] = 0x03 << 6;
      response[MMC_CSD_SIZE_START + 1] = 0xff;
      response[MMC_CSD_SIZE_START + 2] = 0x03;
    });

    sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) -> void {
      *reinterpret_cast<uint32_t*>(&out_data[212]) = htole32(FakeSdmmcDevice::kBlockCount);
      out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
      out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
      out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
      out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
      out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
      out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
      out_data[MMC_EXT_CSD_SEC_FEATURE_SUPPORT] = 0x1
                                                  << MMC_EXT_CSD_SEC_FEATURE_SUPPORT_SEC_GB_CL_EN;
      out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
      out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 0x10;
      out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
    });
  }

  void TearDown() override {
    if (block_device_) {
      zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
      EXPECT_OK(prepare_stop_result);
      EXPECT_OK(dut_.Stop());
    }
  }

  zx_status_t StartDriverForMmc(uint64_t speed_capabilities = 0) {
    return StartDriver(/*is_sd=*/false, speed_capabilities);
  }
  zx_status_t StartDriverForSd(uint64_t speed_capabilities = 0) {
    return StartDriver(/*is_sd=*/true, speed_capabilities);
  }

  zx_status_t StartDriver(bool is_sd, uint64_t speed_capabilities) {
    TestSdmmcRootDevice::use_fidl_ = GetParam();
    TestSdmmcRootDevice::is_sd_ = is_sd;
    if (is_sd) {
      sdmmc_.set_command_callback(SD_SEND_IF_COND,
                                  [](const sdmmc_req_t& req, uint32_t out_response[4]) {
                                    out_response[0] = req.arg & 0xfff;
                                  });

      sdmmc_.set_command_callback(SD_APP_SEND_OP_COND, [](uint32_t out_response[4]) {
        out_response[0] = 0xc000'0000;  // Set busy and CCS bits.
      });

      sdmmc_.set_command_callback(SD_SEND_RELATIVE_ADDR, [](uint32_t out_response[4]) {
        out_response[0] = 0x100;  // Set READY_FOR_DATA bit in SD status.
      });

      sdmmc_.set_command_callback(SDMMC_SEND_CSD, [](uint32_t out_response[4]) {
        out_response[1] = 0x1234'0000;
        out_response[2] = 0x0000'5678;
        out_response[3] = 0x4000'0000;  // Set CSD_STRUCTURE to indicate SDHC/SDXC.
      });
    }

    // Initialize driver test environment.
    fuchsia_driver_framework::DriverStartArgs start_args;
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
    fit::result metadata = fidl::Persist(CreateMetadata(/*removable=*/is_sd, speed_capabilities));
    if (!metadata.is_ok()) {
      return metadata.error_value().status();
    }
    incoming_.SyncCall([&](IncomingNamespace* incoming) mutable {
      auto start_args_result = incoming->node.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client = std::move(start_args_result->outgoing_directory_client);

      ASSERT_OK(incoming->env.Initialize(std::move(start_args_result->incoming_directory_server)));

      incoming->device_server.Init("default", "");
      // Serve metadata.
      ASSERT_OK(incoming->device_server.AddMetadata(DEVICE_METADATA_SDMMC, metadata->data(),
                                                    metadata->size()));
      ASSERT_OK(incoming->device_server.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env.incoming_directory()));
    });

    // Start dut_.
    auto result = runtime_.RunToCompletion(dut_.Start(std::move(start_args)));
    if (!result.is_ok()) {
      return result.status_value();
    }

    const std::unique_ptr<SdmmcBlockDevice>* block_device =
        std::get_if<std::unique_ptr<SdmmcBlockDevice>>(&dut_->child_device());
    if (block_device == nullptr) {
      return ZX_ERR_BAD_STATE;
    }
    block_device_ = block_device->get();

    block_device_->SetBlockInfo(FakeSdmmcDevice::kBlockSize, FakeSdmmcDevice::kBlockCount);
    for (size_t i = 0; i < (FakeSdmmcDevice::kBlockSize / sizeof(kTestData)); i++) {
      test_block_.insert(test_block_.end(), kTestData, kTestData + sizeof(kTestData));
    }

    user_ = GetBlockClient(USER_DATA_PARTITION);
    if (!user_.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }
    if (!is_sd) {
      boot1_ = GetBlockClient(BOOT_PARTITION_1);
      boot2_ = GetBlockClient(BOOT_PARTITION_2);
    }
    return ZX_OK;
  }

  void QueueBlockOps();
  void QueueRpmbRequests();
  fidl::WireSharedClient<fuchsia_hardware_rpmb::Rpmb>& rpmb_client() { return rpmb_client_; }
  std::atomic<bool>& run_threads() { return run_threads_; }

 protected:
  static constexpr size_t kBlockOpSize = BlockOperation::OperationSize(sizeof(block_op_t));
  static constexpr uint32_t kMaxOutstandingOps = 16;

  struct OperationContext {
    zx::vmo vmo;
    fzl::VmoMapper mapper;
    zx_status_t status;
    bool completed;
  };

  struct CallbackContext {
    CallbackContext(uint32_t exp_op) { expected_operations.store(exp_op); }

    std::atomic<uint32_t> expected_operations;
    sync_completion_t completion;
  };

  static void OperationCallback(void* ctx, zx_status_t status, block_op_t* op) {
    auto* const cb_ctx = reinterpret_cast<CallbackContext*>(ctx);

    block::Operation<OperationContext> block_op(op, kBlockOpSize, false);
    block_op.private_storage()->completed = true;
    block_op.private_storage()->status = status;

    if (cb_ctx->expected_operations.fetch_sub(1) == 1) {
      sync_completion_signal(&cb_ctx->completion);
    }
  }

  fuchsia_hardware_sdmmc::wire::SdmmcMetadata CreateMetadata(bool removable,
                                                             uint64_t speed_capabilities) {
    return fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(arena_)
        .speed_capabilities(speed_capabilities)
        .enable_cache(true)
        .removable(removable)
        .max_command_packing(16)
        .use_fidl(false)
        .Build();
  }

  void BindRpmbClient() {
    auto [client_end, server_end] = fidl::Endpoints<fuchsia_hardware_rpmb::Rpmb>::Create();

    binding_ = fidl::BindServer(loop_.dispatcher(), std::move(server_end),
                                block_device_->child_rpmb_device().get());
    ASSERT_OK(loop_.StartThread("rpmb-client-thread"));
    rpmb_client_.Bind(std::move(client_end), loop_.dispatcher());
  }

  void MakeBlockOp(uint8_t opcode, uint32_t length, uint64_t offset,
                   std::optional<block::Operation<OperationContext>>* out_op) {
    *out_op = block::Operation<OperationContext>::Alloc(kBlockOpSize);
    ASSERT_TRUE(*out_op);

    if (opcode == BLOCK_OPCODE_READ || opcode == BLOCK_OPCODE_WRITE) {
      (*out_op)->operation()->rw = {
          .command = {.opcode = opcode, .flags = 0},
          .extra = 0,
          .vmo = ZX_HANDLE_INVALID,
          .length = length,
          .offset_dev = offset,
          .offset_vmo = 0,
      };

      if (length > 0) {
        OperationContext* const ctx = (*out_op)->private_storage();
        const size_t vmo_size = fbl::round_up<size_t, size_t>(length * FakeSdmmcDevice::kBlockSize,
                                                              zx_system_get_page_size());
        ASSERT_OK(ctx->mapper.CreateAndMap(vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
                                           &ctx->vmo));
        ctx->completed = false;
        ctx->status = ZX_OK;
        (*out_op)->operation()->rw.vmo = ctx->vmo.get();
      }
    } else if (opcode == BLOCK_OPCODE_TRIM) {
      (*out_op)->operation()->trim = {
          .command = {.opcode = opcode, .flags = 0},
          .length = length,
          .offset_dev = offset,
      };
    } else {
      (*out_op)->operation()->command = {.opcode = opcode, .flags = 0};
    }
  }

  void FillSdmmc(uint32_t length, uint64_t offset) {
    for (uint32_t i = 0; i < length; i++) {
      sdmmc_.Write((offset + i) * test_block_.size(), test_block_);
    }
  }

  void FillVmo(const fzl::VmoMapper& mapper, uint32_t length, uint64_t offset = 0) {
    auto* ptr = reinterpret_cast<uint8_t*>(mapper.start()) + (offset * test_block_.size());
    for (uint32_t i = 0; i < length; i++, ptr += test_block_.size()) {
      memcpy(ptr, test_block_.data(), test_block_.size());
    }
  }

  void CheckSdmmc(uint32_t length, uint64_t offset) {
    const std::vector<uint8_t> data =
        sdmmc_.Read(offset * test_block_.size(), length * test_block_.size());
    const uint8_t* ptr = data.data();
    for (uint32_t i = 0; i < length; i++, ptr += test_block_.size()) {
      EXPECT_BYTES_EQ(ptr, test_block_.data(), test_block_.size());
    }
  }

  void CheckVmo(const fzl::VmoMapper& mapper, uint32_t length, uint64_t offset = 0) {
    const uint8_t* ptr = reinterpret_cast<uint8_t*>(mapper.start()) + (offset * test_block_.size());
    for (uint32_t i = 0; i < length; i++, ptr += test_block_.size()) {
      EXPECT_BYTES_EQ(ptr, test_block_.data(), test_block_.size());
    }
  }

  void CheckVmoErased(const fzl::VmoMapper& mapper, uint32_t length, uint64_t offset = 0) {
    const size_t blocks_to_u32 = test_block_.size() / sizeof(uint32_t);
    const uint32_t* data = reinterpret_cast<uint32_t*>(mapper.start()) + (offset * blocks_to_u32);
    for (uint32_t i = 0; i < (length * blocks_to_u32); i++) {
      EXPECT_EQ(data[i], 0xffff'ffff);
    }
  }

  ddk::BlockImplProtocolClient GetBlockClient(SdmmcBlockDevice* device, EmmcPartition partition) {
    for (const auto& partition_device : device->child_partition_devices()) {
      if (partition_device->partition() == partition) {
        block_impl_protocol_t proto = {.ops = &partition_device->block_impl_protocol_ops(),
                                       .ctx = partition_device.get()};
        return ddk::BlockImplProtocolClient(&proto);
      }
    }
    return ddk::BlockImplProtocolClient();
  }

  ddk::BlockImplProtocolClient GetBlockClient(EmmcPartition partition) {
    return GetBlockClient(block_device_, partition);
  }

  FakeSdmmcDevice& sdmmc_ = TestSdmmcRootDevice::sdmmc_;
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  fdf_testing::DriverUnderTest<TestSdmmcRootDevice> dut_;
  SdmmcBlockDevice* block_device_;
  ddk::BlockImplProtocolClient user_;
  ddk::BlockImplProtocolClient boot1_;
  ddk::BlockImplProtocolClient boot2_;
  fidl::WireSharedClient<fuchsia_hardware_rpmb::Rpmb> rpmb_client_;
  std::atomic<bool> run_threads_ = true;
  async::Loop loop_;

 private:
  static constexpr uint8_t kTestData[] = {
      // clang-format off
      0xd0, 0x0d, 0x7a, 0xf2, 0xbc, 0x13, 0x81, 0x07,
      0x72, 0xbe, 0x33, 0x5f, 0x21, 0x4e, 0xd7, 0xba,
      0x1b, 0x0c, 0x25, 0xcf, 0x2c, 0x6f, 0x46, 0x3a,
      0x78, 0x22, 0xea, 0x9e, 0xa0, 0x41, 0x65, 0xf8,
      // clang-format on
  };
  static_assert(FakeSdmmcDevice::kBlockSize % sizeof(kTestData) == 0);

  fidl::Arena<> arena_;
  std::vector<uint8_t> test_block_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_rpmb::Rpmb>> binding_;
};

TEST_P(SdmmcBlockDeviceTest, BlockImplQuery) {
  ASSERT_OK(StartDriverForMmc());

  size_t block_op_size;
  block_info_t info;
  user_.Query(&info, &block_op_size);

  EXPECT_EQ(info.block_count, FakeSdmmcDevice::kBlockCount);
  EXPECT_EQ(info.block_size, FakeSdmmcDevice::kBlockSize);
  EXPECT_EQ(block_op_size, kBlockOpSize);
  EXPECT_FALSE(info.flags & FLAG_REMOVABLE);
}

TEST_P(SdmmcBlockDeviceTest, BlockImplQuerySdRemovable) {
  ASSERT_OK(StartDriverForSd());

  size_t block_op_size;
  block_info_t info;
  user_.Query(&info, &block_op_size);

  EXPECT_EQ(info.block_size, FakeSdmmcDevice::kBlockSize);
  EXPECT_EQ(block_op_size, kBlockOpSize);
  EXPECT_TRUE(info.flags & FLAG_REMOVABLE);
}

TEST_P(SdmmcBlockDeviceTest, BlockImplQueue) {
  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 0x8000, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_FLUSH, 0, 0, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 1, 0x400, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 0x2000, &op5));

  CallbackContext ctx(5);

  FillVmo(op1->private_storage()->mapper, 1);
  FillVmo(op2->private_storage()->mapper, 5);
  FillSdmmc(1, 0x400);
  FillSdmmc(10, 0x2000);

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);
  EXPECT_TRUE(op4->private_storage()->completed);
  EXPECT_TRUE(op5->private_storage()->completed);

  EXPECT_OK(op1->private_storage()->status);
  EXPECT_OK(op2->private_storage()->status);
  EXPECT_OK(op3->private_storage()->status);
  EXPECT_OK(op4->private_storage()->status);
  EXPECT_OK(op5->private_storage()->status);

  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(1, 0));
  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(5, 0x8000));
  ASSERT_NO_FATAL_FAILURE(CheckVmo(op4->private_storage()->mapper, 1));
  ASSERT_NO_FATAL_FAILURE(CheckVmo(op5->private_storage()->mapper, 10));
}

TEST_P(SdmmcBlockDeviceTest, BlockImplQueueOutOfRange) {
  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0x100000, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 0x200000, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 8, 0xffff8, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 9, 0xffff8, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 16, 0xffff8, &op5));

  std::optional<block::Operation<OperationContext>> op6;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 0, 0x80000, &op6));

  std::optional<block::Operation<OperationContext>> op7;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0xfffff, &op7));

  CallbackContext ctx(7);

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);
  user_.Queue(op6->operation(), OperationCallback, &ctx);
  user_.Queue(op7->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);
  EXPECT_TRUE(op4->private_storage()->completed);
  EXPECT_TRUE(op5->private_storage()->completed);
  EXPECT_TRUE(op6->private_storage()->completed);
  EXPECT_TRUE(op7->private_storage()->completed);

  EXPECT_NOT_OK(op1->private_storage()->status);
  EXPECT_NOT_OK(op2->private_storage()->status);
  EXPECT_OK(op3->private_storage()->status);
  EXPECT_NOT_OK(op4->private_storage()->status);
  EXPECT_NOT_OK(op5->private_storage()->status);
  EXPECT_NOT_OK(op6->private_storage()->status);
  EXPECT_OK(op7->private_storage()->status);
}

TEST_P(SdmmcBlockDeviceTest, NoCmd12ForSdBlockTransfer) {
  ASSERT_OK(StartDriverForSd());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 0x8000, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_FLUSH, 0, 0, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 1, 0x400, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 0x2000, &op5));

  CallbackContext ctx(5);

  sdmmc_.set_command_callback(SDMMC_READ_MULTIPLE_BLOCK, [](const sdmmc_req_t& req) -> void {
    EXPECT_FALSE(req.cmd_flags & SDMMC_CMD_AUTO12);
  });
  sdmmc_.set_command_callback(SDMMC_WRITE_MULTIPLE_BLOCK, [](const sdmmc_req_t& req) -> void {
    EXPECT_FALSE(req.cmd_flags & SDMMC_CMD_AUTO12);
  });

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  const std::map<uint32_t, uint32_t> command_counts = sdmmc_.command_counts();
  EXPECT_EQ(command_counts.find(SDMMC_STOP_TRANSMISSION), command_counts.end());
}

TEST_P(SdmmcBlockDeviceTest, NoCmd12ForMmcBlockTransfer) {
  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 0x8000, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_FLUSH, 0, 0, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 1, 0x400, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 0x2000, &op5));

  CallbackContext ctx(5);

  sdmmc_.set_command_callback(SDMMC_READ_MULTIPLE_BLOCK, [](const sdmmc_req_t& req) -> void {
    EXPECT_FALSE(req.cmd_flags & SDMMC_CMD_AUTO12);
  });
  sdmmc_.set_command_callback(SDMMC_WRITE_MULTIPLE_BLOCK, [](const sdmmc_req_t& req) -> void {
    EXPECT_FALSE(req.cmd_flags & SDMMC_CMD_AUTO12);
  });

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  const std::map<uint32_t, uint32_t> command_counts = sdmmc_.command_counts();
  EXPECT_EQ(command_counts.find(SDMMC_STOP_TRANSMISSION), command_counts.end());
}

TEST_P(SdmmcBlockDeviceTest, ErrorsPropagate) {
  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_WRITE, 1, FakeSdmmcDevice::kBadRegionStart, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_WRITE, 5, FakeSdmmcDevice::kBadRegionStart | 0x80, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_FLUSH, 0, 0, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_READ, 1, FakeSdmmcDevice::kBadRegionStart | 0x40, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_READ, 10, FakeSdmmcDevice::kBadRegionStart | 0x20, &op5));

  CallbackContext ctx(5);

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);
  EXPECT_TRUE(op4->private_storage()->completed);
  EXPECT_TRUE(op5->private_storage()->completed);

  EXPECT_NOT_OK(op1->private_storage()->status);
  EXPECT_NOT_OK(op2->private_storage()->status);
  EXPECT_OK(op3->private_storage()->status);
  EXPECT_NOT_OK(op4->private_storage()->status);
  EXPECT_NOT_OK(op5->private_storage()->status);
}

TEST_P(SdmmcBlockDeviceTest, SendCmd12OnCommandFailure) {
  sdmmc_.set_host_info({
      .caps = 0,
      .max_transfer_size = fuchsia_hardware_block::wire::kMaxTransferUnbounded,
      .max_transfer_size_non_dma = 0,
  });

  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_WRITE, 1, FakeSdmmcDevice::kBadRegionStart, &op1));
  CallbackContext ctx1(1);

  user_.Queue(op1->operation(), OperationCallback, &ctx1);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx1.completion, zx::duration::infinite().get())); });
  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_EQ(sdmmc_.command_counts().at(SDMMC_STOP_TRANSMISSION), 10);
}

TEST_P(SdmmcBlockDeviceTest, SendCmd12OnCommandFailureWhenAutoCmd12) {
  sdmmc_.set_host_info({
      .caps = SDMMC_HOST_CAP_AUTO_CMD12,
      .max_transfer_size = fuchsia_hardware_block::wire::kMaxTransferUnbounded,
      .max_transfer_size_non_dma = 0,
  });

  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_WRITE, 1, FakeSdmmcDevice::kBadRegionStart, &op2));
  CallbackContext ctx2(1);

  user_.Queue(op2->operation(), OperationCallback, &ctx2);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx2.completion, zx::duration::infinite().get())); });
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_EQ(sdmmc_.command_counts().at(SDMMC_STOP_TRANSMISSION), 10);
}

TEST_P(SdmmcBlockDeviceTest, Trim) {
  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 10, 100, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_FLUSH, 0, 0, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 100, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_TRIM, 1, 103, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 100, &op5));

  std::optional<block::Operation<OperationContext>> op6;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_TRIM, 3, 106, &op6));

  std::optional<block::Operation<OperationContext>> op7;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 100, &op7));

  FillVmo(op1->private_storage()->mapper, 10);

  CallbackContext ctx(7);

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);
  user_.Queue(op6->operation(), OperationCallback, &ctx);
  user_.Queue(op7->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  ASSERT_NO_FATAL_FAILURE(CheckVmo(op3->private_storage()->mapper, 10, 0));

  ASSERT_NO_FATAL_FAILURE(CheckVmo(op5->private_storage()->mapper, 3, 0));
  ASSERT_NO_FATAL_FAILURE(CheckVmoErased(op5->private_storage()->mapper, 1, 3));
  ASSERT_NO_FATAL_FAILURE(CheckVmo(op5->private_storage()->mapper, 6, 4));

  ASSERT_NO_FATAL_FAILURE(CheckVmo(op7->private_storage()->mapper, 3, 0));
  ASSERT_NO_FATAL_FAILURE(CheckVmoErased(op7->private_storage()->mapper, 1, 3));
  ASSERT_NO_FATAL_FAILURE(CheckVmo(op7->private_storage()->mapper, 2, 4));
  ASSERT_NO_FATAL_FAILURE(CheckVmoErased(op7->private_storage()->mapper, 3, 6));
  ASSERT_NO_FATAL_FAILURE(CheckVmo(op7->private_storage()->mapper, 1, 9));

  EXPECT_OK(op1->private_storage()->status);
  EXPECT_OK(op2->private_storage()->status);
  EXPECT_OK(op3->private_storage()->status);
  EXPECT_OK(op4->private_storage()->status);
  EXPECT_OK(op5->private_storage()->status);
  EXPECT_OK(op6->private_storage()->status);
  EXPECT_OK(op7->private_storage()->status);
}

TEST_P(SdmmcBlockDeviceTest, TrimErrors) {
  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_TRIM, 10, 10, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_TRIM, 10, FakeSdmmcDevice::kBadRegionStart | 0x40, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(
      MakeBlockOp(BLOCK_OPCODE_TRIM, 10, FakeSdmmcDevice::kBadRegionStart - 5, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_TRIM, 10, 100, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_TRIM, 10, 110, &op5));

  sdmmc_.set_command_callback(MMC_ERASE_GROUP_START,
                              [](const sdmmc_req_t& req, uint32_t out_response[4]) {
                                if (req.arg == 100) {
                                  out_response[0] |= MMC_STATUS_ERASE_SEQ_ERR;
                                }
                              });

  sdmmc_.set_command_callback(MMC_ERASE_GROUP_END,
                              [](const sdmmc_req_t& req, uint32_t out_response[4]) {
                                if (req.arg == 119) {
                                  out_response[0] |= MMC_STATUS_ADDR_OUT_OF_RANGE;
                                }
                              });

  CallbackContext ctx(5);

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_OK(op1->private_storage()->status);
  EXPECT_NOT_OK(op2->private_storage()->status);
  EXPECT_NOT_OK(op3->private_storage()->status);
  EXPECT_NOT_OK(op4->private_storage()->status);
  EXPECT_NOT_OK(op5->private_storage()->status);
}

TEST_P(SdmmcBlockDeviceTest, OnlyUserDataPartitionExists) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 0;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());

  EXPECT_EQ(block_device_->child_partition_devices().size(), 1);
  EXPECT_EQ(block_device_->child_partition_devices()[0]->partition(), USER_DATA_PARTITION);
  EXPECT_EQ(block_device_->child_rpmb_device(), nullptr);
}

TEST_P(SdmmcBlockDeviceTest, BootPartitionsExistButNotUsed) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 2;
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 1;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());

  EXPECT_EQ(block_device_->child_partition_devices().size(), 1);
  EXPECT_EQ(block_device_->child_partition_devices()[0]->partition(), USER_DATA_PARTITION);
  EXPECT_EQ(block_device_->child_rpmb_device(), nullptr);
}

TEST_P(SdmmcBlockDeviceTest, WithBootPartitions) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 1;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());

  EXPECT_EQ(block_device_->child_partition_devices().size(), 3);
  EXPECT_EQ(block_device_->child_partition_devices()[0]->partition(), USER_DATA_PARTITION);
  EXPECT_EQ(block_device_->child_partition_devices()[1]->partition(), BOOT_PARTITION_1);
  EXPECT_EQ(block_device_->child_partition_devices()[2]->partition(), BOOT_PARTITION_2);
  EXPECT_EQ(block_device_->child_rpmb_device(), nullptr);
}

TEST_P(SdmmcBlockDeviceTest, WithBootAndRpmbPartitions) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_RPMB_SIZE_MULT] = 1;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 1;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());
  BindRpmbClient();

  EXPECT_EQ(block_device_->child_partition_devices().size(), 3);
  EXPECT_EQ(block_device_->child_partition_devices()[0]->partition(), USER_DATA_PARTITION);
  EXPECT_EQ(block_device_->child_partition_devices()[1]->partition(), BOOT_PARTITION_1);
  EXPECT_EQ(block_device_->child_partition_devices()[2]->partition(), BOOT_PARTITION_2);
  EXPECT_NE(block_device_->child_rpmb_device(), nullptr);
}

TEST_P(SdmmcBlockDeviceTest, CompleteTransactions) {
  ASSERT_OK(StartDriverForMmc());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 0x8000, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_FLUSH, 0, 0, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 1, 0x400, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 0x2000, &op5));

  CallbackContext ctx(5);

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);
  EXPECT_TRUE(op4->private_storage()->completed);
  EXPECT_TRUE(op5->private_storage()->completed);
}

TEST_P(SdmmcBlockDeviceTest, CompleteTransactionsOnStop) {
  ASSERT_OK(StartDriverForMmc());
  // Stop the worker dispatcher so queued requests don't get completed.
  block_device_->StopWorkerDispatcher();

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 0x8000, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_FLUSH, 0, 0, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 1, 0x400, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 10, 0x2000, &op5));

  CallbackContext ctx(5);

  user_.Queue(op1->operation(), OperationCallback, &ctx);
  user_.Queue(op2->operation(), OperationCallback, &ctx);
  user_.Queue(op3->operation(), OperationCallback, &ctx);
  user_.Queue(op4->operation(), OperationCallback, &ctx);
  user_.Queue(op5->operation(), OperationCallback, &ctx);

  zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
  EXPECT_OK(prepare_stop_result);
  EXPECT_OK(dut_.Stop());
  block_device_ = nullptr;

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);
  EXPECT_TRUE(op4->private_storage()->completed);
  EXPECT_TRUE(op5->private_storage()->completed);
}

TEST_P(SdmmcBlockDeviceTest, ProbeMmcSendStatusRetry) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_DEVICE_TYPE] = 1 << 4;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 1;
  });
  sdmmc_.set_command_callback(SDMMC_SEND_STATUS, [](const sdmmc_req_t& req) {
    // Fail two out of three times during ProbeMmc, and then succeed for
    // SdmmcBlockDevice::AddDevice.
    static uint32_t call_count = 0;
    if (++call_count % 3 == 0 || call_count > 9) {
      return ZX_OK;
    } else {
      return ZX_ERR_IO_DATA_INTEGRITY;
    }
  });

  EXPECT_OK(StartDriverForMmc());
}

TEST_P(SdmmcBlockDeviceTest, ProbeMmcSendStatusFail) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_DEVICE_TYPE] = 1 << 4;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 1;
  });
  sdmmc_.set_command_callback(SDMMC_SEND_STATUS,
                              [](const sdmmc_req_t& req) { return ZX_ERR_IO_DATA_INTEGRITY; });

  EXPECT_NOT_OK(StartDriverForMmc());
}

TEST_P(SdmmcBlockDeviceTest, QueryBootPartitions) {
  ASSERT_OK(StartDriverForMmc());

  ASSERT_TRUE(boot1_.is_valid());
  ASSERT_TRUE(boot2_.is_valid());

  size_t boot1_op_size, boot2_op_size;
  block_info_t boot1_info, boot2_info;
  boot1_.Query(&boot1_info, &boot1_op_size);
  boot2_.Query(&boot2_info, &boot2_op_size);

  EXPECT_EQ(boot1_info.block_count, (0x10 * 128 * 1024) / FakeSdmmcDevice::kBlockSize);
  EXPECT_EQ(boot2_info.block_count, (0x10 * 128 * 1024) / FakeSdmmcDevice::kBlockSize);

  EXPECT_EQ(boot1_info.block_size, FakeSdmmcDevice::kBlockSize);
  EXPECT_EQ(boot2_info.block_size, FakeSdmmcDevice::kBlockSize);

  EXPECT_EQ(boot1_op_size, kBlockOpSize);
  EXPECT_EQ(boot2_op_size, kBlockOpSize);
}

TEST_P(SdmmcBlockDeviceTest, AccessBootPartitions) {
  ASSERT_OK(StartDriverForMmc());

  ASSERT_TRUE(boot1_.is_valid());
  ASSERT_TRUE(boot2_.is_valid());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 0, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 5, 10, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 10, 500, &op3));

  FillVmo(op1->private_storage()->mapper, 1);
  FillSdmmc(5, 10);
  FillVmo(op3->private_storage()->mapper, 10);

  CallbackContext ctx(1);

  sdmmc_.set_command_callback(MMC_SWITCH, [](const sdmmc_req_t& req) {
    const uint32_t index = (req.arg >> 16) & 0xff;
    const uint32_t value = (req.arg >> 8) & 0xff;
    EXPECT_EQ(index, MMC_EXT_CSD_PARTITION_CONFIG);
    EXPECT_EQ(value, 0xa8 | BOOT_PARTITION_1);
  });

  boot1_.Queue(op1->operation(), OperationCallback, &ctx);
  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  ctx.expected_operations.store(1);
  sync_completion_reset(&ctx.completion);

  sdmmc_.set_command_callback(MMC_SWITCH, [](const sdmmc_req_t& req) {
    const uint32_t index = (req.arg >> 16) & 0xff;
    const uint32_t value = (req.arg >> 8) & 0xff;
    EXPECT_EQ(index, MMC_EXT_CSD_PARTITION_CONFIG);
    EXPECT_EQ(value, 0xa8 | BOOT_PARTITION_2);
  });

  boot2_.Queue(op2->operation(), OperationCallback, &ctx);
  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  ctx.expected_operations.store(1);
  sync_completion_reset(&ctx.completion);

  sdmmc_.set_command_callback(MMC_SWITCH, [](const sdmmc_req_t& req) {
    const uint32_t index = (req.arg >> 16) & 0xff;
    const uint32_t value = (req.arg >> 8) & 0xff;
    EXPECT_EQ(index, MMC_EXT_CSD_PARTITION_CONFIG);
    EXPECT_EQ(value, 0xa8 | USER_DATA_PARTITION);
  });

  user_.Queue(op3->operation(), OperationCallback, &ctx);
  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);

  EXPECT_OK(op1->private_storage()->status);
  EXPECT_OK(op2->private_storage()->status);
  EXPECT_OK(op3->private_storage()->status);

  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(1, 0));
  ASSERT_NO_FATAL_FAILURE(CheckVmo(op2->private_storage()->mapper, 5));
  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(10, 500));
}

TEST_P(SdmmcBlockDeviceTest, BootPartitionRepeatedAccess) {
  ASSERT_OK(StartDriverForMmc());

  ASSERT_TRUE(boot2_.is_valid());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 1, 0, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 10, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 2, 5, &op3));

  FillSdmmc(1, 0);
  FillVmo(op2->private_storage()->mapper, 5);
  FillVmo(op3->private_storage()->mapper, 2);

  CallbackContext ctx(1);

  sdmmc_.set_command_callback(MMC_SWITCH, [](const sdmmc_req_t& req) {
    const uint32_t index = (req.arg >> 16) & 0xff;
    const uint32_t value = (req.arg >> 8) & 0xff;
    EXPECT_EQ(index, MMC_EXT_CSD_PARTITION_CONFIG);
    EXPECT_EQ(value, 0xa8 | BOOT_PARTITION_2);
  });

  boot2_.Queue(op1->operation(), OperationCallback, &ctx);
  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  ctx.expected_operations.store(2);
  sync_completion_reset(&ctx.completion);

  // Repeated accesses to one partition should not generate more than one MMC_SWITCH command.
  sdmmc_.set_command_callback(MMC_SWITCH, [](const sdmmc_req_t& req) { FAIL(); });

  boot2_.Queue(op2->operation(), OperationCallback, &ctx);
  boot2_.Queue(op3->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);

  EXPECT_OK(op1->private_storage()->status);
  EXPECT_OK(op2->private_storage()->status);
  EXPECT_OK(op3->private_storage()->status);

  ASSERT_NO_FATAL_FAILURE(CheckVmo(op1->private_storage()->mapper, 1));
  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(5, 10));
  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(2, 5));
}

TEST_P(SdmmcBlockDeviceTest, AccessBootPartitionOutOfRange) {
  ASSERT_OK(StartDriverForMmc());

  ASSERT_TRUE(boot1_.is_valid());

  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 4096, &op1));

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 8, 4088, &op2));

  std::optional<block::Operation<OperationContext>> op3;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 9, 4088, &op3));

  std::optional<block::Operation<OperationContext>> op4;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 16, 4088, &op4));

  std::optional<block::Operation<OperationContext>> op5;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_READ, 0, 2048, &op5));

  std::optional<block::Operation<OperationContext>> op6;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 1, 4095, &op6));

  CallbackContext ctx(6);

  boot1_.Queue(op1->operation(), OperationCallback, &ctx);
  boot1_.Queue(op2->operation(), OperationCallback, &ctx);
  boot1_.Queue(op3->operation(), OperationCallback, &ctx);
  boot1_.Queue(op4->operation(), OperationCallback, &ctx);
  boot1_.Queue(op5->operation(), OperationCallback, &ctx);
  boot1_.Queue(op6->operation(), OperationCallback, &ctx);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_TRUE(op3->private_storage()->completed);
  EXPECT_TRUE(op4->private_storage()->completed);
  EXPECT_TRUE(op5->private_storage()->completed);
  EXPECT_TRUE(op6->private_storage()->completed);

  EXPECT_NOT_OK(op1->private_storage()->status);
  EXPECT_OK(op2->private_storage()->status);
  EXPECT_NOT_OK(op3->private_storage()->status);
  EXPECT_NOT_OK(op4->private_storage()->status);
  EXPECT_NOT_OK(op5->private_storage()->status);
  EXPECT_OK(op6->private_storage()->status);
}

TEST_P(SdmmcBlockDeviceTest, ProbeUsesPrefsHs) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_DEVICE_TYPE] = 0b0101'0110;  // Card supports HS200/400, HS/DDR.
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  const uint64_t speed_capabilities =
      static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs200) |
      static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs400) |
      static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHsddr);
  EXPECT_OK(StartDriverForMmc(speed_capabilities));

  EXPECT_EQ(sdmmc_.timing(), SDMMC_TIMING_HS);
}

TEST_P(SdmmcBlockDeviceTest, ProbeUsesPrefsHsDdr) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_DEVICE_TYPE] = 0b0101'0110;  // Card supports HS200/400, HS/DDR.
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  const uint64_t speed_capabilities =
      static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs200) |
      static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs400);
  EXPECT_OK(StartDriverForMmc(speed_capabilities));

  EXPECT_EQ(sdmmc_.timing(), SDMMC_TIMING_HSDDR);
}

TEST_P(SdmmcBlockDeviceTest, ProbeHs400) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_DEVICE_TYPE] = 0b0101'0110;  // Card supports HS200/400, HS/DDR.
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  uint32_t timing = MMC_EXT_CSD_HS_TIMING_LEGACY;
  sdmmc_.set_command_callback(SDMMC_SEND_STATUS, [&](const sdmmc_req_t& req) {
    // SDMMC_SEND_STATUS is the first command sent to the card after MMC_SWITCH. When initializing
    // HS400 mode the host sets the card timing to HS200 and then to HS, and should change the
    // timing and frequency on the host before issuing SDMMC_SEND_STATUS.
    if (timing == MMC_EXT_CSD_HS_TIMING_HS) {
      EXPECT_EQ(sdmmc_.timing(), SDMMC_TIMING_HS);
      EXPECT_LE(sdmmc_.bus_freq(), 52'000'000);
    }
  });

  sdmmc_.set_command_callback(MMC_SWITCH, [&](const sdmmc_req_t& req) {
    const uint32_t index = (req.arg >> 16) & 0xff;
    if (index == MMC_EXT_CSD_HS_TIMING) {
      const uint32_t value = (req.arg >> 8) & 0xff;
      EXPECT_GE(value, MMC_EXT_CSD_HS_TIMING_LEGACY);
      EXPECT_LE(value, MMC_EXT_CSD_HS_TIMING_HS400);
      timing = value;
    }
  });

  EXPECT_OK(StartDriverForMmc());

  EXPECT_EQ(sdmmc_.timing(), SDMMC_TIMING_HS400);
}

TEST_P(SdmmcBlockDeviceTest, ProbeSd) {
  ASSERT_OK(StartDriverForSd());

  size_t block_op_size;
  block_info_t info;
  user_.Query(&info, &block_op_size);

  EXPECT_EQ(info.block_size, 512);
  EXPECT_EQ(info.block_count, 0x38'1235 * 1024ul);
}

TEST_P(SdmmcBlockDeviceTest, RpmbPartition) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_RPMB_SIZE_MULT] = 0x74;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
    out_data[MMC_EXT_CSD_REL_WR_SEC_C] = 1;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 0x10;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());
  BindRpmbClient();

  sync_completion_t completion;
  rpmb_client_->GetDeviceInfo().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::GetDeviceInfo>& result) {
        if (!result.ok()) {
          FAIL("GetDeviceInfo failed: %s", result.error().FormatDescription().c_str());
          return;
        }
        auto* response = result.Unwrap();
        EXPECT_TRUE(response->info.is_emmc_info());
        EXPECT_EQ(response->info.emmc_info().rpmb_size, 0x74);
        EXPECT_EQ(response->info.emmc_info().reliable_write_sector_count, 1);
        sync_completion_signal(&completion);
      });

  sync_completion_wait(&completion, zx::duration::infinite().get());
  sync_completion_reset(&completion);

  fzl::VmoMapper tx_frames_mapper;
  fzl::VmoMapper rx_frames_mapper;

  zx::vmo tx_frames;
  zx::vmo rx_frames;

  ASSERT_OK(tx_frames_mapper.CreateAndMap(512 * 4, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
                                          &tx_frames));
  ASSERT_OK(rx_frames_mapper.CreateAndMap(512 * 4, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
                                          &rx_frames));

  fuchsia_hardware_rpmb::wire::Request write_read_request = {};
  ASSERT_OK(tx_frames.duplicate(ZX_RIGHT_SAME_RIGHTS, &write_read_request.tx_frames.vmo));

  write_read_request.tx_frames.offset = 1024;
  write_read_request.tx_frames.size = 1024;
  FillVmo(tx_frames_mapper, 2, 2);

  fuchsia_mem::wire::Range rx_frames_range = {};
  ASSERT_OK(rx_frames.duplicate(ZX_RIGHT_SAME_RIGHTS, &rx_frames_range.vmo));
  rx_frames_range.offset = 512;
  rx_frames_range.size = 1536;
  write_read_request.rx_frames =
      fidl::ObjectView<fuchsia_mem::wire::Range>::FromExternal(&rx_frames_range);

  sdmmc_.set_command_callback(MMC_SWITCH, [](const sdmmc_req_t& req) {
    const uint32_t index = (req.arg >> 16) & 0xff;
    const uint32_t value = (req.arg >> 8) & 0xff;
    EXPECT_EQ(index, MMC_EXT_CSD_PARTITION_CONFIG);
    EXPECT_EQ(value, 0xa8 | RPMB_PARTITION);
  });

  rpmb_client_->Request(std::move(write_read_request))
      .ThenExactlyOnce([&](fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::Request>& result) {
        if (!result.ok()) {
          FAIL("Request failed: %s", result.error().FormatDescription().c_str());
          return;
        }
        EXPECT_FALSE(result->is_error());
        sync_completion_signal(&completion);
      });

  sync_completion_wait(&completion, zx::duration::infinite().get());
  sync_completion_reset(&completion);

  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(2, 0));
  // The first two blocks were written by the RPMB write request, and read back by the read request.
  ASSERT_NO_FATAL_FAILURE(CheckVmo(rx_frames_mapper, 2, 1));

  fuchsia_hardware_rpmb::wire::Request write_request = {};
  ASSERT_OK(tx_frames.duplicate(ZX_RIGHT_SAME_RIGHTS, &write_request.tx_frames.vmo));

  write_request.tx_frames.offset = 0;
  write_request.tx_frames.size = 2048;
  FillVmo(tx_frames_mapper, 4, 0);

  // Repeated accesses to one partition should not generate more than one MMC_SWITCH command.
  sdmmc_.set_command_callback(MMC_SWITCH, []([[maybe_unused]] const sdmmc_req_t& req) { FAIL(); });

  sdmmc_.set_command_callback(SDMMC_SET_BLOCK_COUNT, [](const sdmmc_req_t& req) {
    EXPECT_TRUE(req.arg & MMC_SET_BLOCK_COUNT_RELIABLE_WRITE);
  });

  rpmb_client_->Request(std::move(write_request))
      .ThenExactlyOnce([&](fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::Request>& result) {
        if (!result.ok()) {
          FAIL("Request failed: %s", result.error().FormatDescription().c_str());
          return;
        }
        EXPECT_FALSE(result->is_error());
        sync_completion_signal(&completion);
      });

  sync_completion_wait(&completion, zx::duration::infinite().get());
  sync_completion_reset(&completion);

  ASSERT_NO_FATAL_FAILURE(CheckSdmmc(4, 0));
}

TEST_P(SdmmcBlockDeviceTest, RpmbRequestLimit) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_RPMB_SIZE_MULT] = 0x74;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
    out_data[MMC_EXT_CSD_REL_WR_SEC_C] = 1;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 0x10;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());
  BindRpmbClient();
  block_device_->StopWorkerDispatcher();

  zx::vmo tx_frames;
  ASSERT_OK(zx::vmo::create(512, 0, &tx_frames));

  for (int i = 0; i < 16; i++) {
    fuchsia_hardware_rpmb::wire::Request request = {};
    ASSERT_OK(tx_frames.duplicate(ZX_RIGHT_SAME_RIGHTS, &request.tx_frames.vmo));
    request.tx_frames.offset = 0;
    request.tx_frames.size = 512;
    rpmb_client_->Request(std::move(request))
        .ThenExactlyOnce(
            [&]([[maybe_unused]] fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::Request>&
                    result) {});
  }

  fuchsia_hardware_rpmb::wire::Request error_request = {};
  ASSERT_OK(tx_frames.duplicate(ZX_RIGHT_SAME_RIGHTS, &error_request.tx_frames.vmo));
  error_request.tx_frames.offset = 0;
  error_request.tx_frames.size = 512;

  sync_completion_t error_completion;
  rpmb_client_->Request(std::move(error_request))
      .ThenExactlyOnce([&](fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::Request>& result) {
        if (!result.ok()) {
          FAIL("Request failed: %s", result.error().FormatDescription().c_str());
          return;
        }
        EXPECT_TRUE(result->is_error());
        sync_completion_signal(&error_completion);
      });

  sync_completion_wait(&error_completion, zx::duration::infinite().get());
}

void SdmmcBlockDeviceTest::QueueBlockOps() {
  struct BlockContext {
    sync_completion_t completion = {};
    block::OperationPool<> free_ops;
    block::OperationQueue<> outstanding_ops;
    std::atomic<uint32_t> outstanding_op_count = 0;
  } context;

  const auto op_callback = [](void* ctx, zx_status_t status, block_op_t* bop) {
    EXPECT_OK(status);

    auto& op_ctx = *reinterpret_cast<BlockContext*>(ctx);
    block::Operation<> op(bop, kBlockOpSize);
    EXPECT_TRUE(op_ctx.outstanding_ops.erase(&op));
    op_ctx.free_ops.push(std::move(op));

    // Wake up the block op thread when half the outstanding operations have been completed.
    if (op_ctx.outstanding_op_count.fetch_sub(1) == kMaxOutstandingOps / 2) {
      sync_completion_signal(&op_ctx.completion);
    }
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1024, 0, &vmo));

  // Populate the free op list.
  for (uint32_t i = 0; i < kMaxOutstandingOps; i++) {
    std::optional<block::Operation<>> op = block::Operation<>::Alloc(kBlockOpSize);
    ASSERT_TRUE(op);
    context.free_ops.push(*std::move(op));
  }

  while (run_threads_.load()) {
    for (uint32_t i = context.outstanding_op_count.load(); i < kMaxOutstandingOps;
         i = context.outstanding_op_count.fetch_add(1) + 1) {
      // Move an op from the free list to the outstanding list. The callback will erase the op from
      // the outstanding list and move it back to the free list.
      std::optional<block::Operation<>> op = context.free_ops.pop();
      ASSERT_TRUE(op);

      op->operation()->rw = {
          .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0}, .vmo = vmo.get(), .length = 1};

      block_op_t* const bop = op->operation();
      context.outstanding_ops.push(*std::move(op));
      user_.Queue(bop, op_callback, &context);
    }

    sync_completion_wait(&context.completion, zx::duration::infinite().get());
    sync_completion_reset(&context.completion);
  }

  while (context.outstanding_op_count.load() > 0) {
  }
}

void SdmmcBlockDeviceTest::QueueRpmbRequests() {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512, 0, &vmo));

  std::atomic<uint32_t> outstanding_op_count = 0;
  sync_completion_t completion;

  while (run_threads_.load()) {
    for (uint32_t i = outstanding_op_count.load(); i < kMaxOutstandingOps;
         i = outstanding_op_count.fetch_add(1) + 1) {
      fuchsia_hardware_rpmb::wire::Request request = {};
      EXPECT_OK(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &request.tx_frames.vmo));
      request.tx_frames.offset = 0;
      request.tx_frames.size = 512;

      rpmb_client_->Request(std::move(request))
          .ThenExactlyOnce(
              [&](fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::Request>& result) {
                if (!result.ok()) {
                  FAIL("Request failed: %s", result.error().FormatDescription().c_str());
                  return;
                }

                EXPECT_FALSE(result->is_error());
                if (outstanding_op_count.fetch_sub(1) == kMaxOutstandingOps / 2) {
                  sync_completion_signal(&completion);
                }
              });
    }

    sync_completion_wait(&completion, zx::duration::infinite().get());
    sync_completion_reset(&completion);
  }

  while (outstanding_op_count.load() > 0) {
  }
}

TEST_P(SdmmcBlockDeviceTest, RpmbRequestsGetToRun) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    *reinterpret_cast<uint32_t*>(&out_data[212]) = htole32(FakeSdmmcDevice::kBlockCount);
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_RPMB_SIZE_MULT] = 0x10;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 0x10;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());
  BindRpmbClient();

  ASSERT_TRUE(boot1_.is_valid());
  ASSERT_TRUE(boot2_.is_valid());

  thrd_t rpmb_thread;
  EXPECT_EQ(
      thrd_create_with_name(
          &rpmb_thread,
          [](void* ctx) -> int {
            auto test = reinterpret_cast<SdmmcBlockDeviceTest*>(ctx);

            zx::vmo vmo;
            if (zx::vmo::create(512, 0, &vmo) != ZX_OK) {
              return thrd_error;
            }

            std::atomic<uint32_t> ops_completed = 0;
            sync_completion_t completion;

            for (uint32_t i = 0; i < kMaxOutstandingOps; i++) {
              fuchsia_hardware_rpmb::wire::Request request = {};
              EXPECT_OK(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &request.tx_frames.vmo));
              request.tx_frames.offset = 0;
              request.tx_frames.size = 512;

              test->rpmb_client()
                  ->Request(std::move(request))
                  .ThenExactlyOnce(
                      [&](fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::Request>& result) {
                        if (!result.ok()) {
                          FAIL("Request failed: %s", result.error().FormatDescription().c_str());
                          return;
                        }

                        EXPECT_FALSE(result->is_error());
                        if ((ops_completed.fetch_add(1) + 1) == kMaxOutstandingOps) {
                          sync_completion_signal(&completion);
                        }
                      });
            }

            sync_completion_wait(&completion, zx::duration::infinite().get());

            test->run_threads().store(false);

            return thrd_success;
          },
          this, "rpmb-queue-thread"),
      thrd_success);

  // Choose to run QueueBlockOps() using the foreground dispatcher, while
  // fuchsia_hardware_sdmmc::Sdmmc is being served by the background dispatcher.
  runtime_.PerformBlockingWork([this] { QueueBlockOps(); });
  EXPECT_EQ(thrd_join(rpmb_thread, nullptr), thrd_success);
}

TEST_P(SdmmcBlockDeviceTest, BlockOpsGetToRun) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    *reinterpret_cast<uint32_t*>(&out_data[212]) = htole32(FakeSdmmcDevice::kBlockCount);
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_RPMB_SIZE_MULT] = 0x10;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 0x10;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());
  BindRpmbClient();

  ASSERT_TRUE(boot1_.is_valid());
  ASSERT_TRUE(boot2_.is_valid());

  thrd_t rpmb_thread;
  EXPECT_EQ(thrd_create_with_name(
                &rpmb_thread,
                [](void* ctx) -> int {
                  reinterpret_cast<SdmmcBlockDeviceTest*>(ctx)->QueueRpmbRequests();
                  return thrd_success;
                },
                this, "rpmb-queue-thread"),
            thrd_success);

  block::OperationPool<> outstanding_ops;

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1024, 0, &vmo));

  struct BlockContext {
    std::atomic<uint32_t> ops_completed = 0;
    sync_completion_t completion = {};
  } context;

  const auto op_callback = [](void* ctx, zx_status_t status, [[maybe_unused]] block_op_t* bop) {
    EXPECT_OK(status);

    auto& op_ctx = *reinterpret_cast<BlockContext*>(ctx);
    if ((op_ctx.ops_completed.fetch_add(1) + 1) == kMaxOutstandingOps) {
      sync_completion_signal(&op_ctx.completion);
    }
  };

  for (uint32_t i = 0; i < kMaxOutstandingOps; i++) {
    std::optional<block::Operation<>> op = block::Operation<>::Alloc(kBlockOpSize);
    ASSERT_TRUE(op);

    op->operation()->rw = {
        .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0}, .vmo = vmo.get(), .length = 1};

    block_op_t* const bop = op->operation();
    outstanding_ops.push(*std::move(op));
    user_.Queue(bop, op_callback, &context);
  }

  runtime_.PerformBlockingWork(
      [&] { sync_completion_wait(&context.completion, zx::duration::infinite().get()); });

  run_threads_.store(false);
  EXPECT_EQ(thrd_join(rpmb_thread, nullptr), thrd_success);
}

TEST_P(SdmmcBlockDeviceTest, GetRpmbClient) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    *reinterpret_cast<uint32_t*>(&out_data[212]) = htole32(FakeSdmmcDevice::kBlockCount);
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_RPMB_SIZE_MULT] = 0x74;
    out_data[MMC_EXT_CSD_PARTITION_CONFIG] = 0xa8;
    out_data[MMC_EXT_CSD_REL_WR_SEC_C] = 1;
    out_data[MMC_EXT_CSD_PARTITION_SWITCH_TIME] = 0;
    out_data[MMC_EXT_CSD_BOOT_SIZE_MULT] = 0x10;
    out_data[MMC_EXT_CSD_GENERIC_CMD6_TIME] = 0;
  });

  ASSERT_OK(StartDriverForMmc());
  BindRpmbClient();

  sync_completion_t completion;
  rpmb_client_->GetDeviceInfo().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_rpmb::Rpmb::GetDeviceInfo>& result) {
        if (!result.ok()) {
          FAIL("GetDeviceInfo failed: %s", result.error().FormatDescription().c_str());
          return;
        }
        auto* response = result.Unwrap();
        EXPECT_TRUE(response->info.is_emmc_info());
        EXPECT_EQ(response->info.emmc_info().rpmb_size, 0x74);
        EXPECT_EQ(response->info.emmc_info().reliable_write_sector_count, 1);
        sync_completion_signal(&completion);
      });

  sync_completion_wait(&completion, zx::duration::infinite().get());
  sync_completion_reset(&completion);
}

TEST_P(SdmmcBlockDeviceTest, Inspect) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    *reinterpret_cast<uint32_t*>(&out_data[212]) = htole32(FakeSdmmcDevice::kBlockCount);
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A] = 3;
    out_data[MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B] = 7;
    out_data[MMC_EXT_CSD_MAX_PACKED_WRITES] = 63;
    out_data[MMC_EXT_CSD_MAX_PACKED_READS] = 62;
  });

  ASSERT_OK(StartDriverForMmc());

  const zx::vmo inspect_vmo = block_device_->inspector().DuplicateVmo();
  ASSERT_TRUE(inspect_vmo.is_valid());

  // IO error count should be zero after initialization.
  inspect::InspectTestHelper inspector;
  inspector.ReadInspect(inspect_vmo);

  const inspect::Hierarchy* root = inspector.hierarchy().GetByPath({"sdmmc_core"});
  ASSERT_NOT_NULL(root);

  const auto* io_errors = root->node().get_property<inspect::UintPropertyValue>("io_errors");
  ASSERT_NOT_NULL(io_errors);
  EXPECT_EQ(io_errors->value(), 0);

  const auto* io_retries = root->node().get_property<inspect::UintPropertyValue>("io_retries");
  ASSERT_NOT_NULL(io_retries);
  EXPECT_EQ(io_retries->value(), 0);

  const auto* type_a_lifetime =
      root->node().get_property<inspect::UintPropertyValue>("type_a_lifetime_used");
  ASSERT_NOT_NULL(type_a_lifetime);
  EXPECT_EQ(type_a_lifetime->value(), 3);

  const auto* type_b_lifetime =
      root->node().get_property<inspect::UintPropertyValue>("type_b_lifetime_used");
  ASSERT_NOT_NULL(type_b_lifetime);
  EXPECT_EQ(type_b_lifetime->value(), 7);

  const auto* max_lifetime =
      root->node().get_property<inspect::UintPropertyValue>("max_lifetime_used");
  ASSERT_NOT_NULL(max_lifetime);
  EXPECT_EQ(max_lifetime->value(), 7);

  const auto* cache_size = root->node().get_property<inspect::UintPropertyValue>("cache_size_bits");
  ASSERT_NOT_NULL(cache_size);
  EXPECT_EQ(cache_size->value(), 1024 * 0x12345678ull);

  const auto* cache_enabled =
      root->node().get_property<inspect::BoolPropertyValue>("cache_enabled");
  ASSERT_NOT_NULL(cache_enabled);
  EXPECT_TRUE(cache_enabled->value());

  const auto* max_packed_reads =
      root->node().get_property<inspect::UintPropertyValue>("max_packed_reads");
  ASSERT_NOT_NULL(max_packed_reads);
  EXPECT_EQ(max_packed_reads->value(), 62);

  const auto* max_packed_writes =
      root->node().get_property<inspect::UintPropertyValue>("max_packed_writes");
  ASSERT_NOT_NULL(max_packed_writes);
  EXPECT_EQ(max_packed_writes->value(), 63);

  const auto* max_packed_reads_effective =
      root->node().get_property<inspect::UintPropertyValue>("max_packed_reads_effective");
  ASSERT_NOT_NULL(max_packed_reads_effective);
  EXPECT_EQ(max_packed_reads_effective->value(), 16);

  const auto* max_packed_writes_effective =
      root->node().get_property<inspect::UintPropertyValue>("max_packed_writes_effective");
  ASSERT_NOT_NULL(max_packed_writes_effective);
  EXPECT_EQ(max_packed_writes_effective->value(), 16);

  // IO error count should be a successful block op.
  std::optional<block::Operation<OperationContext>> op1;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 0x8000, &op1));

  CallbackContext ctx1(1);

  user_.Queue(op1->operation(), OperationCallback, &ctx1);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx1.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op1->private_storage()->completed);
  EXPECT_OK(op1->private_storage()->status);

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"sdmmc_core"});
  ASSERT_NOT_NULL(root);

  io_errors = root->node().get_property<inspect::UintPropertyValue>("io_errors");
  ASSERT_NOT_NULL(io_errors);
  EXPECT_EQ(io_errors->value(), 0);

  io_retries = root->node().get_property<inspect::UintPropertyValue>("io_retries");
  ASSERT_NOT_NULL(io_retries);
  EXPECT_EQ(io_retries->value(), 0);

  // IO error count should be incremented after a failed block op.
  sdmmc_.set_command_callback(SDMMC_WRITE_MULTIPLE_BLOCK,
                              [](const sdmmc_req_t& req) -> zx_status_t { return ZX_ERR_IO; });

  std::optional<block::Operation<OperationContext>> op2;
  ASSERT_NO_FATAL_FAILURE(MakeBlockOp(BLOCK_OPCODE_WRITE, 5, 0x8000, &op2));

  CallbackContext ctx2(1);

  user_.Queue(op2->operation(), OperationCallback, &ctx2);

  runtime_.PerformBlockingWork(
      [&] { EXPECT_OK(sync_completion_wait(&ctx2.completion, zx::duration::infinite().get())); });

  EXPECT_TRUE(op2->private_storage()->completed);
  EXPECT_NOT_OK(op2->private_storage()->status);

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"sdmmc_core"});
  ASSERT_NOT_NULL(root);

  io_errors = root->node().get_property<inspect::UintPropertyValue>("io_errors");
  ASSERT_NOT_NULL(io_errors);
  EXPECT_EQ(io_errors->value(), 1);

  io_retries = root->node().get_property<inspect::UintPropertyValue>("io_retries");
  ASSERT_NOT_NULL(io_retries);
  EXPECT_EQ(io_retries->value(), 9);
}

TEST_P(SdmmcBlockDeviceTest, InspectInvalidLifetime) {
  sdmmc_.set_command_callback(MMC_SEND_EXT_CSD, [](cpp20::span<uint8_t> out_data) {
    *reinterpret_cast<uint32_t*>(&out_data[212]) = htole32(FakeSdmmcDevice::kBlockCount);
    out_data[MMC_EXT_CSD_CACHE_CTRL] = 1;
    out_data[MMC_EXT_CSD_CACHE_SIZE_LSB] = 0x78;
    out_data[MMC_EXT_CSD_CACHE_SIZE_250] = 0x56;
    out_data[MMC_EXT_CSD_CACHE_SIZE_251] = 0x34;
    out_data[MMC_EXT_CSD_CACHE_SIZE_MSB] = 0x12;
    out_data[MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A] = 0xe;
    out_data[MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B] = 6;
  });

  ASSERT_OK(StartDriverForMmc());

  const zx::vmo inspect_vmo = block_device_->inspector().DuplicateVmo();
  ASSERT_TRUE(inspect_vmo.is_valid());

  inspect::InspectTestHelper inspector;
  inspector.ReadInspect(inspect_vmo);

  const inspect::Hierarchy* root = inspector.hierarchy().GetByPath({"sdmmc_core"});
  ASSERT_NOT_NULL(root);

  const auto* type_a_lifetime =
      root->node().get_property<inspect::UintPropertyValue>("type_a_lifetime_used");
  ASSERT_NOT_NULL(type_a_lifetime);
  EXPECT_EQ(type_a_lifetime->value(), 0xc);  // Value out of range should be normalized.

  const auto* type_b_lifetime =
      root->node().get_property<inspect::UintPropertyValue>("type_b_lifetime_used");
  ASSERT_NOT_NULL(type_b_lifetime);
  EXPECT_EQ(type_b_lifetime->value(), 6);

  const auto* max_lifetime =
      root->node().get_property<inspect::UintPropertyValue>("max_lifetime_used");
  ASSERT_NOT_NULL(max_lifetime);
  EXPECT_EQ(max_lifetime->value(), 6);  // Only the valid value should be used.
}

TEST_P(SdmmcBlockDeviceTest, PowerSuspendResume) {
  sdmmc_.set_command_callback(MMC_SLEEP_AWAKE,
                              [](const sdmmc_req_t& req, uint32_t out_response[4]) {
                                const bool sleep = (req.arg >> 15) & 0x1;
                                if (sleep) {
                                  out_response[0] |= MMC_STATUS_CURRENT_STATE_STBY;
                                } else {
                                  out_response[0] |= MMC_STATUS_CURRENT_STATE_SLP;
                                }
                              });

  ASSERT_OK(StartDriverForMmc());

  const zx::vmo inspect_vmo = block_device_->inspector().DuplicateVmo();
  ASSERT_TRUE(inspect_vmo.is_valid());

  inspect::InspectTestHelper inspector;
  inspector.ReadInspect(inspect_vmo);

  const inspect::Hierarchy* root = inspector.hierarchy().GetByPath({"sdmmc_core"});
  ASSERT_NOT_NULL(root);

  const auto* power_suspended =
      root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_FALSE(power_suspended->value());

  EXPECT_OK(block_device_->SuspendPower());

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"sdmmc_core"});
  ASSERT_NOT_NULL(root);

  power_suspended = root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_TRUE(power_suspended->value());

  EXPECT_OK(block_device_->ResumePower());

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"sdmmc_core"});
  ASSERT_NOT_NULL(root);

  power_suspended = root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_FALSE(power_suspended->value());
}

INSTANTIATE_TEST_SUITE_P(SdmmcProtocolUsingFidlTest, SdmmcBlockDeviceTest, zxtest::Bool());

}  // namespace sdmmc

FUCHSIA_DRIVER_EXPORT(sdmmc::TestSdmmcRootDevice);
