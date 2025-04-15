// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "block.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/driver/testing/cpp/minimal_compat_environment.h>
#include <lib/fake-bti/bti.h>
#include <lib/sync/completion.h>
#include <lib/virtio/backends/fake.h>

#include <condition_variable>
#include <cstdint>
#include <memory>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace {

constexpr uint64_t kCapacity = 1024;
constexpr uint64_t kSizeMax = 4000;
constexpr uint64_t kSegMax = 1024;
constexpr uint64_t kBlkSize = 1024;
constexpr uint64_t kVmoOffsetBlocks = 1;
const uint16_t kRingSize = 128;  // Should match block.h

// Fake virtio 'backend' for a virtio-block device.
class FakeBackendForBlock : public virtio::FakeBackend {
 public:
  FakeBackendForBlock(zx_handle_t fake_bti)
      : virtio::FakeBackend({{0, 1024}}), fake_bti_(fake_bti) {
    // Fill out a block config:
    virtio_blk_config config;
    memset(&config, 0, sizeof(config));
    config.capacity = kCapacity;
    config.size_max = kSizeMax;
    config.seg_max = kSegMax;
    config.blk_size = kBlkSize;

    for (uint16_t i = 0; i < sizeof(config); ++i) {
      AddClassRegister(i, reinterpret_cast<uint8_t*>(&config)[i]);
    }
  }

  void set_status(uint8_t status) { status_ = status; }

  uint64_t ReadFeatures() override {
    uint64_t bitmap = FakeBackend::ReadFeatures();

    // Declare support for VIRTIO_F_VERSION_1.
    bitmap |= VIRTIO_F_VERSION_1;

    return bitmap;
  }

  void RingKick(uint16_t ring_index) override {
    FakeBackend::RingKick(ring_index);

    fake_bti_pinned_vmo_info_t vmos[16];
    size_t count;
    ASSERT_OK(fake_bti_get_pinned_vmos(fake_bti_, vmos, 16, &count));
    ASSERT_LE(size_t{2}, count);

    union __PACKED Used {
      vring_used head;
      struct __PACKED {
        uint8_t header[sizeof(vring_used)];
        vring_used_elem elements[kRingSize];
      };
    } used;
    union __PACKED Avail {
      vring_avail head;
      struct __PACKED {
        uint8_t header[sizeof(vring_avail)];
        uint16_t ring[kRingSize];
      };
    } avail;

    // This assumes that the ring is in the first VMO.
    ASSERT_OK(zx_vmo_read(vmos[0].vmo, &used, vmos[0].offset + used_offset_, sizeof(used)));
    ASSERT_OK(zx_vmo_read(vmos[0].vmo, &avail, vmos[0].offset + avail_offset_, sizeof(avail)));

    if (avail.head.idx != used.head.idx) {
      ASSERT_EQ(avail.head.idx, used.head.idx + 1);  // We can only handle one queued entry.

      size_t index = used.head.idx & (kRingSize - 1);

      // Read the descriptors.
      vring_desc descriptors[kRingSize];
      ASSERT_OK(zx_vmo_read(vmos[0].vmo, descriptors, vmos[0].offset + desc_offset_,
                            sizeof(descriptors)));

      // Find the last descriptor.
      vring_desc* desc = &descriptors[avail.ring[index]];
      uint16_t count = 1;
      uint16_t data_descriptor_idx = UINT16_MAX;
      while (desc->flags & VRING_DESC_F_NEXT) {
        if (desc->addr % zx_system_get_page_size() == kBlkSize * kVmoOffsetBlocks) {
          data_descriptor_idx = count;
        }
        desc = &descriptors[desc->next];
        ++count;
      }
      // The second-last descriptor describes the first page of data transfer (the first descriptor
      // is the head descriptor).
      if (data_descriptor_idx != UINT16_MAX) {
        ZX_ASSERT_MSG(data_descriptor_idx == count - 1,
                      "The second-last descriptor should point to data");
      }

      // It should be the status descriptor.
      ASSERT_EQ(uint32_t{1}, desc->len);

      // This assumes the results are in the second VMO.
      size_t offset = vmos[1].offset + desc->addr - FAKE_BTI_PHYS_ADDR;
      ASSERT_OK(zx_vmo_write(vmos[1].vmo, &status_, offset, sizeof(status_)));

      used.elements[index].id = avail.ring[index];
      used.elements[index].len = count;

      ++used.head.idx;

      ASSERT_OK(zx_vmo_write(vmos[0].vmo, &used, vmos[0].offset + used_offset_, sizeof(used)));

      // Trigger an interrupt.
      uint8_t isr_status;
      ReadRegister(kISRStatus, &isr_status);
      isr_status |= VIRTIO_ISR_QUEUE_INT;
      SetRegister(kISRStatus, isr_status);

      std::scoped_lock lock(mutex_);
      interrupt_ = true;
      cond_.notify_all();
    }
  }

  zx_status_t SetRing(uint16_t index, uint16_t count, zx_paddr_t pa_desc, zx_paddr_t pa_avail,
                      zx_paddr_t pa_used) override {
    FakeBackend::SetRing(index, count, pa_desc, pa_avail, pa_used);
    used_offset_ = pa_used - FAKE_BTI_PHYS_ADDR;
    avail_offset_ = pa_avail - FAKE_BTI_PHYS_ADDR;
    desc_offset_ = pa_desc - FAKE_BTI_PHYS_ADDR;
    ZX_ASSERT(count == kRingSize);
    return ZX_OK;
  }

  zx_status_t InterruptValid() override {
    std::scoped_lock lock(mutex_);
    return terminate_ ? ZX_ERR_CANCELED : ZX_OK;
  }

  zx::result<uint32_t> WaitForInterrupt() override {
    std::unique_lock<std::mutex> lock(mutex_);
    for (;;) {
      if (terminate_)
        return zx::error(ZX_ERR_CANCELED);
      if (interrupt_)
        return zx::ok(0);
      cond_.wait(lock);
    }
  }

  void InterruptAck(uint32_t key) override {
    std::scoped_lock lock(mutex_);
    interrupt_ = false;
  }

  void Terminate() override {
    std::scoped_lock lock(mutex_);
    terminate_ = true;
    cond_.notify_all();
  }

 private:
  // The vring offsets.
  size_t used_offset_ = 0;
  size_t avail_offset_ = 0;
  size_t desc_offset_ = 0;

  zx_handle_t fake_bti_;

  std::mutex mutex_;
  std::condition_variable cond_;
  bool terminate_ = false;
  bool interrupt_ = false;

  // The status returned for any operations.
  uint8_t status_ = VIRTIO_BLK_S_OK;
};

class TestBlockDriver : public virtio::BlockDriver {
 public:
  // Modify to configure the behaviour of this test driver.
  static uint8_t backend_status_;

  TestBlockDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : BlockDriver(std::move(start_args), std::move(dispatcher)) {}

 protected:
  zx::result<std::unique_ptr<virtio::BlockDevice>> CreateBlockDevice() override {
    zx::bti bti(ZX_HANDLE_INVALID);
    zx_status_t status = fake_bti_create(bti.reset_and_get_address());
    if (status != ZX_OK) {
      return zx::error(status);
    }
    auto backend = std::make_unique<FakeBackendForBlock>(bti.get());
    backend->set_status(backend_status_);

    return zx::ok(
        std::make_unique<virtio::BlockDevice>(std::move(bti), std::move(backend), logger()));
  }
};

uint8_t TestBlockDriver::backend_status_;

class TestConfig final {
 public:
  using DriverType = TestBlockDriver;
  using EnvironmentType = fdf_testing::MinimalCompatEnvironment;
};

// Provides control primitives for tests that issue IO requests to the device.
class BlockDriverTest : public ::testing::Test {
 public:
  void StartDriver(uint8_t status = VIRTIO_BLK_S_OK) {
    TestBlockDriver::backend_status_ = status;
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fdf_testing::ForegroundDriverTest<TestConfig>& driver_test() { return driver_test_; }

  virtio::block_txn_t TestReadCommand(uint32_t tranfser_blocks = 1) {
    virtio::block_txn_t txn;
    memset(&txn, 0, sizeof(txn));
    txn.op.rw.command.opcode = BLOCK_OPCODE_READ;
    txn.op.rw.length = tranfser_blocks;
    txn.op.rw.offset_vmo = kVmoOffsetBlocks;
    return txn;
  }

  virtio::block_txn_t TestTrimCommand(uint32_t trim_blocks = 1) {
    virtio::block_txn_t txn;
    memset(&txn, 0, sizeof(txn));
    txn.op.rw.command.opcode = BLOCK_OPCODE_TRIM;
    txn.op.rw.length = trim_blocks;
    return txn;
  }

  static void CompletionCb(void* cookie, zx_status_t status, block_op_t* op) {
    BlockDriverTest* operation = reinterpret_cast<BlockDriverTest*>(cookie);
    operation->operation_status_ = status;
    sync_completion_signal(&operation->event_);
  }

  bool Wait() {
    zx_status_t status = sync_completion_wait(&event_, ZX_SEC(5));
    sync_completion_reset(&event_);
    return status == ZX_OK;
  }

  zx_status_t OperationStatus() { return operation_status_; }

 protected:
  std::unique_ptr<virtio::BlockDevice> device_;

 private:
  fdf_testing::ForegroundDriverTest<TestConfig> driver_test_;
  sync_completion_t event_;
  zx_status_t operation_status_;
};

// Tests trivial attempts to queue one operation.
TEST_F(BlockDriverTest, QueueOne) {
  StartDriver();

  virtio::block_txn_t txn = TestReadCommand(0);
  driver_test().driver()->BlockImplQueue(reinterpret_cast<block_op_t*>(&txn),
                                         &BlockDriverTest::CompletionCb, this);
  ASSERT_TRUE(Wait());
  ASSERT_EQ(ZX_ERR_OUT_OF_RANGE, OperationStatus());

  txn = TestReadCommand(kCapacity * 10);
  driver_test().driver()->BlockImplQueue(reinterpret_cast<block_op_t*>(&txn),
                                         &BlockDriverTest::CompletionCb, this);
  ASSERT_TRUE(Wait());
  ASSERT_EQ(ZX_ERR_OUT_OF_RANGE, OperationStatus());
}

TEST_F(BlockDriverTest, CheckQuery) {
  StartDriver();

  block_info_t info;
  size_t operation_size;
  driver_test().driver()->BlockImplQuery(&info, &operation_size);
  ASSERT_EQ(info.block_size, kBlkSize);
  ASSERT_EQ(info.block_count, kCapacity);
  ASSERT_GE(info.max_transfer_size, zx_system_get_page_size());
  ASSERT_GT(operation_size, sizeof(block_op_t));
}

TEST_F(BlockDriverTest, ReadOk) {
  StartDriver();

  virtio::block_txn_t txn = TestReadCommand();
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  txn.op.rw.vmo = vmo.get();
  driver_test().driver()->BlockImplQueue(reinterpret_cast<block_op_t*>(&txn),
                                         &BlockDriverTest::CompletionCb, this);
  ASSERT_TRUE(Wait());
  ASSERT_EQ(ZX_OK, OperationStatus());
}

TEST_F(BlockDriverTest, ReadError) {
  StartDriver(VIRTIO_BLK_S_IOERR);

  virtio::block_txn_t txn = TestReadCommand();
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  txn.op.rw.vmo = vmo.get();
  driver_test().driver()->BlockImplQueue(reinterpret_cast<block_op_t*>(&txn),
                                         &BlockDriverTest::CompletionCb, this);
  ASSERT_TRUE(Wait());
  ASSERT_EQ(ZX_ERR_IO, OperationStatus());
}

TEST_F(BlockDriverTest, Trim) {
  StartDriver();

  virtio::block_txn_t txn = TestTrimCommand();
  driver_test().driver()->BlockImplQueue(reinterpret_cast<block_op_t*>(&txn),
                                         &BlockDriverTest::CompletionCb, this);
  ASSERT_TRUE(Wait());
  ASSERT_OK(OperationStatus());
}

FUCHSIA_DRIVER_EXPORT(TestBlockDriver);

}  // anonymous namespace
