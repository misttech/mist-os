// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <bitset>

#include "unit-lib.h"

namespace ufs {
class BlockOpTest : public UfsTest {
 public:
  void SetUp() override {
    UfsTest::SetUp();

    while (dut_->block_devs().empty()) {
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }

    const auto& block_devs = dut_->block_devs();
    block_device_ = block_devs.at(0).at(0).get();
    block_device_->BlockImplQuery(&info_, &op_size_);
  }

 protected:
  scsi::BlockDevice* block_device_;
  block_info_t info_;
  uint64_t op_size_;
};

static void FillRandom(uint8_t* buf, size_t size) {
  for (size_t i = 0; i < size; ++i) {
    buf[i] = static_cast<uint8_t>(rand());
  }
}

TEST_F(BlockOpTest, ReadTest) {
  const uint8_t kTestLun = 0;

  char buf[ufs_mock_device::kMockBlockSize];
  std::strncpy(buf, "test", sizeof(buf));
  ASSERT_OK(mock_device_.BufferWrite(kTestLun, buf, 1, 0));

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));
  auto block_op = std::make_unique<uint8_t[]>(op_size_);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_READ,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  block_device_->BlockImplQueue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);

  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ, 0, vmo, 0, ufs_mock_device::kMockBlockSize,
                                       &vaddr));
  char* mapped_vaddr = reinterpret_cast<char*>(vaddr);
  ASSERT_EQ(std::memcmp(buf, mapped_vaddr, ufs_mock_device::kMockBlockSize), 0);
  ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, ufs_mock_device::kMockBlockSize));
}

TEST_F(BlockOpTest, WriteTest) {
  const uint8_t kTestLun = 0;

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));

  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       ufs_mock_device::kMockBlockSize, &vaddr));
  char* mapped_vaddr = reinterpret_cast<char*>(vaddr);
  std::strncpy(mapped_vaddr, "test", ufs_mock_device::kMockBlockSize);

  auto block_op = std::make_unique<uint8_t[]>(op_size_);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_WRITE,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  block_device_->BlockImplQueue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);

  char buf[ufs_mock_device::kMockBlockSize];
  ASSERT_OK(mock_device_.BufferRead(kTestLun, buf, 1, 0));

  ASSERT_EQ(std::memcmp(buf, mapped_vaddr, ufs_mock_device::kMockBlockSize), 0);
  ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, ufs_mock_device::kMockBlockSize));
}

TEST_F(BlockOpTest, FuaWriteTest) {
  const uint8_t kTestLun = 0;

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));

  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       ufs_mock_device::kMockBlockSize, &vaddr));
  char* mapped_vaddr = reinterpret_cast<char*>(vaddr);
  std::strncpy(mapped_vaddr, "test", ufs_mock_device::kMockBlockSize);

  auto block_op = std::make_unique<uint8_t[]>(op_size_);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_WRITE,
                      .flags = BLOCK_IO_FLAG_FORCE_ACCESS,  // FUA flag
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  block_device_->BlockImplQueue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);

  // Check that the FUA bit is set.
  ScsiCommandUpiu scsi_upiu(
      *dut_->GetTransferRequestProcessor().GetRequestList().GetDescriptorBuffer<CommandUpiuData>(
          0));
  scsi::Write10CDB* scsi_cdb =
      reinterpret_cast<scsi::Write10CDB*>(scsi_upiu.GetData<CommandUpiuData>()->cdb);
  ASSERT_EQ(scsi_cdb->force_unit_access(), true);

  char buf[ufs_mock_device::kMockBlockSize];
  ASSERT_OK(mock_device_.BufferRead(kTestLun, buf, 1, 0));

  ASSERT_EQ(std::memcmp(buf, mapped_vaddr, ufs_mock_device::kMockBlockSize), 0);
  ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, ufs_mock_device::kMockBlockSize));
}

TEST_F(BlockOpTest, FlushTest) {
  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  auto block_op = std::make_unique<uint8_t[]>(op_size_);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  op->rw.command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};
  block_device_->BlockImplQueue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);

  // Check that the FLUSH operation is correctly converted to a SYNCHRONIZE CACHE 10 command.
  ScsiCommandUpiu scsi_upiu(
      *dut_->GetTransferRequestProcessor().GetRequestList().GetDescriptorBuffer<CommandUpiuData>(
          0));
  ASSERT_EQ(scsi_upiu.GetOpcode(), scsi::Opcode::SYNCHRONIZE_CACHE_10);
}

TEST_F(BlockOpTest, TrimTest) {
  const uint8_t kTestLun = 0;

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));

  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       ufs_mock_device::kMockBlockSize, &vaddr));
  char* mapped_vaddr = reinterpret_cast<char*>(vaddr);
  std::strncpy(mapped_vaddr, "test", ufs_mock_device::kMockBlockSize);

  // Send WRITE operation.
  auto block_op = std::make_unique<uint8_t[]>(op_size_);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_WRITE,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  block_device_->BlockImplQueue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
  sync_completion_reset(&done);

  char buf[ufs_mock_device::kMockBlockSize];
  ASSERT_OK(mock_device_.BufferRead(kTestLun, buf, 1, 0));
  ASSERT_EQ(std::memcmp(buf, mapped_vaddr, ufs_mock_device::kMockBlockSize), 0);

  // Send TRIM operation.
  auto block_op_trim = std::make_unique<uint8_t[]>(op_size_);
  auto trim_op = reinterpret_cast<block_op_t*>(block_op_trim.get());
  *trim_op = {
      .trim =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_TRIM,
                  },
              .length = 1,
              .offset_dev = 0,
          },
  };
  block_device_->BlockImplQueue(trim_op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
  sync_completion_reset(&done);

  // Check that the trimmed block is zero.
  ASSERT_OK(mock_device_.BufferRead(kTestLun, buf, 1, 0));

  char zero_buf[ufs_mock_device::kMockBlockSize];
  std::memset(zero_buf, 0, ufs_mock_device::kMockBlockSize);
  ASSERT_EQ(std::memcmp(buf, zero_buf, ufs_mock_device::kMockBlockSize), 0);

  ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, ufs_mock_device::kMockBlockSize));
}

TEST_F(BlockOpTest, IoRangeExceptionTest) {
  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  auto exception_callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    // exception_callback expect I/O range error.
    EXPECT_EQ(status, ZX_ERR_OUT_OF_RANGE);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));
  auto block_op = std::make_unique<uint8_t[]>(op_size_);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());

  // Normal I/O. No errors occur.
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_READ,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  block_device_->BlockImplQueue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
  sync_completion_reset(&done);

  // If the I/O length is zero, an I/O range error occurs.
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_READ,
                  },
              .vmo = vmo.get(),
              .length = 0,
              .offset_dev = 0,
          },
  };
  block_device_->BlockImplQueue(op, exception_callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
  sync_completion_reset(&done);

  // If the I/O length exceeds the total block count, an I/O range error occurs.
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_READ,
                  },
              .vmo = vmo.get(),
              .length = static_cast<uint32_t>(info_.block_count) + 1,
              .offset_dev = 0,
          },
  };
  block_device_->BlockImplQueue(op, exception_callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
  sync_completion_reset(&done);

  // If the request offset does not fit within total block count, an I/O range error occurs.
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_READ,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = static_cast<uint32_t>(info_.block_count),
          },
  };
  block_device_->BlockImplQueue(op, exception_callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
  sync_completion_reset(&done);

  // If the request offset and length does not fit within total block count, an I/O range error
  // occurs.
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_READ,
                  },
              .vmo = vmo.get(),
              .length = 2,
              .offset_dev = static_cast<uint32_t>(info_.block_count) - 1,
          },
  };
  block_device_->BlockImplQueue(op, exception_callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
  sync_completion_reset(&done);
}

TEST_F(BlockOpTest, TransferSizeTest) {
  const uint8_t kTestLun = 0;

  ASSERT_EQ(kMaxTransferSize1MiB, info_.max_transfer_size);

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status) << "Failed with block_length";
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kMaxTransferSize1MiB, 0, &vmo));

  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       kMaxTransferSize1MiB, &vaddr));
  uint8_t* mapped_vaddr = reinterpret_cast<uint8_t*>(vaddr);
  FillRandom(mapped_vaddr, kMaxTransferSize1MiB);

  auto buffer = std::make_unique<uint8_t[]>(kMaxTransferSize1MiB);
  const uint32_t max_block_count = kMaxTransferSize1MiB / ufs_mock_device::kMockBlockSize;

  // Test on 4KiB, 8KiB, 16KiB, 32KiB, 64KiB, 128KiB, 256KiB, 512KiB, and 1MiB transfer length.
  for (uint32_t block_count = 1; block_count <= max_block_count; block_count *= 2) {
    auto block_op = std::make_unique<uint8_t[]>(op_size_);
    auto op = reinterpret_cast<block_op_t*>(block_op.get());
    *op = {
        .rw =
            {
                .command =
                    {
                        .opcode = BLOCK_OPCODE_WRITE,
                    },
                .vmo = vmo.get(),
                .length = block_count,
                .offset_dev = 0,
                .offset_vmo = 0,
            },
    };
    block_device_->BlockImplQueue(op, callback, &done);
    sync_completion_wait(&done, ZX_TIME_INFINITE);
    sync_completion_reset(&done);

    std::memset(buffer.get(), 0, kMaxTransferSize1MiB);
    EXPECT_OK(mock_device_.BufferRead(kTestLun, buffer.get(), block_count, 0));

    EXPECT_EQ(
        std::memcmp(buffer.get(), mapped_vaddr, block_count * ufs_mock_device::kMockBlockSize), 0);
  }

  ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, kMaxTransferSize1MiB));
}

TEST_F(BlockOpTest, MultiQueueDepthWriteTest) {
  const uint8_t kTestLun = 0;

  // Disable IoLoop completion
  dut_->DisableCompletion();

  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  // Test on 1, 2, 4, 8, 16, and 31 queue depth.
  // One of the 32 slots is dedicated to the admin command, so the maximum queue depth is 31.
  std::vector<uint8_t> queue_depth_list{1, 2, 4, 8, 16, 31};
  for (auto queue_depth : queue_depth_list) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize * queue_depth, 0, &vmo));

    zx_vaddr_t vaddr;
    ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                         ufs_mock_device::kMockBlockSize * queue_depth, &vaddr));
    uint8_t* mapped_vaddr = reinterpret_cast<uint8_t*>(vaddr);
    FillRandom(mapped_vaddr, ufs_mock_device::kMockBlockSize * queue_depth);

    auto block_ops = std::make_unique<uint8_t[]>(op_size_ * queue_depth);
    std::vector<sync_completion_t> done(queue_depth);

    for (uint32_t i = 0; i < queue_depth; ++i) {
      auto op = reinterpret_cast<block_op_t*>(block_ops.get() + (op_size_ * i));
      *op = {
          .rw =
              {
                  .command =
                      {
                          .opcode = BLOCK_OPCODE_WRITE,
                      },
                  .vmo = vmo.get(),
                  .length = 1,
                  .offset_dev = i,
                  .offset_vmo = i,
              },
      };
      block_device_->BlockImplQueue(op, callback, &done[i]);
    }

    // Wait until the slot is used up to the desired queue depth.
    constexpr uint32_t kMultiQueueTimeoutUs = 1000000;
    auto wait_for_scheduled = [&]() -> bool {
      return GetSlotStateCount(SlotState::kScheduled) == queue_depth;
    };
    fbl::String submission_timeout_message = "Timeout waiting for submission";
    ASSERT_OK(dut_->WaitWithTimeout(wait_for_scheduled, kMultiQueueTimeoutUs,
                                    submission_timeout_message));

    // Wait for mock device write I/O is completed.
    auto wait_for_completion = [&]() -> bool {
      std::bitset<32> notification =
          UtrListCompletionNotificationReg::Get().ReadFrom(&dut_->GetMmio()).notification();
      return notification.count() == queue_depth;
    };
    fbl::String completion_timeout_message = "Timeout waiting for completion";
    ASSERT_OK(dut_->WaitWithTimeout(wait_for_completion, kMultiQueueTimeoutUs,
                                    completion_timeout_message));

    dut_->ProcessIoCompletions();
    ASSERT_EQ(GetSlotStateCount(SlotState::kFree),
              dut_->GetTransferRequestProcessor().GetRequestList().GetSlotCount());

    for (uint32_t i = 0; i < queue_depth; ++i) {
      sync_completion_wait(&done[i], ZX_TIME_INFINITE);
      sync_completion_reset(&done[i]);
    }

    auto buf = std::make_unique<uint8_t[]>(ufs_mock_device::kMockBlockSize * queue_depth);
    ASSERT_OK(mock_device_.BufferRead(kTestLun, buf.get(), queue_depth, 0));

    ASSERT_EQ(std::memcmp(buf.get(), mapped_vaddr, ufs_mock_device::kMockBlockSize), 0);
    ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, ufs_mock_device::kMockBlockSize));
  }
}

}  // namespace ufs
