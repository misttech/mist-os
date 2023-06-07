// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <lib/fit/function.h>
#include <lib/scsi/controller.h>
#include <lib/scsi/disk.h>
#include <sys/types.h>
#include <zircon/listnode.h>

#include <map>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace scsi {

// Controller for test; allows us to set expectations and fakes command responses.
class ControllerForTest : public Controller {
 public:
  using IOCallbackType = fit::function<zx_status_t(uint8_t, uint16_t, iovec, bool, iovec)>;

  ~ControllerForTest() { ASSERT_EQ(times_, 0); }

  // Init the state required for testing async IOs.
  zx_status_t AsyncIoInit() {
    {
      fbl::AutoLock lock(&lock_);
      list_initialize(&queued_ios_);
      worker_thread_exit_ = false;
    }
    auto cb = [](void* arg) -> int { return static_cast<ControllerForTest*>(arg)->WorkerThread(); };
    if (thrd_create_with_name(&worker_thread_, cb, this, "scsi-test-controller") != thrd_success) {
      printf("%s: Failed to create worker thread\n", __FILE__);
      return ZX_ERR_INTERNAL;
    }
    return ZX_OK;
  }

  // De-Init the state required for testing async IOs.
  void AsyncIoRelease() {
    {
      fbl::AutoLock lock(&lock_);
      worker_thread_exit_ = true;
      cv_.Signal();
    }
    thrd_join(worker_thread_, nullptr);
    list_node_t* node;
    list_node_t* temp_node;
    fbl::AutoLock lock(&lock_);
    list_for_every_safe(&queued_ios_, node, temp_node) {
      auto* io = containerof(node, struct queued_io, node);
      list_delete(node);
      free(io);
    }
  }

  size_t BlockOpSize() override {
    // No additional metadata required for each command transaction.
    return sizeof(DiskOp);
  }

  void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                           uint32_t block_size_bytes, DiskOp* disk_op) override {
    // In the caller, enqueue the request for the worker thread,
    // poke the worker thread and return. The worker thread, on
    // waking up, will do the actual IO and call the callback.
    auto* io = reinterpret_cast<struct queued_io*>(new queued_io);
    io->target = target;
    io->lun = lun;
    // The cdb is allocated on the stack in the scsi::Disk's BlockImplQueue.
    // So make a copy of that locally, and point to that instead
    memcpy(reinterpret_cast<void*>(&io->cdbptr), cdb.iov_base, cdb.iov_len);
    io->cdb.iov_base = &io->cdbptr;
    io->cdb.iov_len = cdb.iov_len;
    io->is_write = is_write;
    io->data_vmo = zx::unowned_vmo(disk_op->op.rw.vmo);
    io->vmo_offset_bytes = disk_op->op.rw.offset_vmo * block_size_bytes;
    io->transfer_bytes = disk_op->op.rw.length * block_size_bytes;
    io->disk_op = disk_op;
    fbl::AutoLock lock(&lock_);
    list_add_tail(&queued_ios_, &io->node);
    cv_.Signal();
  }

  zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                 iovec data) override {
    EXPECT_TRUE(do_io_);
    EXPECT_GT(times_, 0);

    if (!do_io_ || times_ == 0) {
      return ZX_ERR_INTERNAL;
    }

    auto status = do_io_(target, lun, cdb, is_write, data);
    if (--times_ == 0) {
      decltype(do_io_) empty;
      do_io_.swap(empty);
    }
    return status;
  }

  void ExpectCall(IOCallbackType do_io, int times) {
    do_io_.swap(do_io);
    times_ = times;
  }

 private:
  IOCallbackType do_io_;
  int times_ = 0;

  int WorkerThread() {
    fbl::AutoLock lock(&lock_);
    while (true) {
      if (worker_thread_exit_ == true)
        return ZX_OK;
      // While non-empty, remove requests and execute them
      list_node_t* node;
      list_node_t* temp_node;
      list_for_every_safe(&queued_ios_, node, temp_node) {
        auto* io = containerof(node, struct queued_io, node);
        list_delete(node);
        zx_status_t status;
        std::unique_ptr<uint8_t[]> temp_buffer;

        if (io->data_vmo->is_valid()) {
          temp_buffer = std::make_unique<uint8_t[]>(io->transfer_bytes);
          // In case of WRITE command, populate the temp buffer with data from VMO.
          if (io->is_write) {
            status = zx_vmo_read(io->data_vmo->get(), temp_buffer.get(), io->vmo_offset_bytes,
                                 io->transfer_bytes);
            if (status != ZX_OK) {
              io->disk_op->Complete(status);
              delete io;
              continue;
            }
          }
        }

        status = ExecuteCommandSync(io->target, io->lun, io->cdb, io->is_write,
                                    {temp_buffer.get(), io->transfer_bytes});

        // In case of READ command, populate the VMO with data from temp buffer.
        if (status == ZX_OK && !io->is_write && io->data_vmo->is_valid()) {
          status = zx_vmo_write(io->data_vmo->get(), temp_buffer.get(), io->vmo_offset_bytes,
                                io->transfer_bytes);
        }

        io->disk_op->Complete(status);
        delete io;
      }
      cv_.Wait(&lock_);
    }
    return ZX_OK;
  }

  struct queued_io {
    list_node_t node;
    uint8_t target;
    uint16_t lun;
    // Deep copy of the CDB.
    union {
      Read16CDB readcdb;
      Write16CDB writecdb;
    } cdbptr;
    iovec cdb;
    bool is_write;
    zx::unowned_vmo data_vmo;
    zx_off_t vmo_offset_bytes;
    size_t transfer_bytes;
    DiskOp* disk_op;
  };

  // These are the state for testing Async IOs.
  // The test enqueues Async IOs and pokes the worker thread, which
  // does the IO, and calls back.
  fbl::Mutex lock_;
  fbl::ConditionVariable cv_;
  thrd_t worker_thread_;
  bool worker_thread_exit_ __TA_GUARDED(lock_);
  list_node_t queued_ios_ __TA_GUARDED(lock_);
};

class DiskTest : public zxtest::Test {
 public:
  static constexpr uint8_t kTarget = 5;
  static constexpr uint16_t kLun = 1;
  static constexpr int kTransferSize = 32 * 1024;
  static constexpr uint32_t kBlockSize = 512;
  static constexpr uint64_t kFakeBlocks = 0x128000000;

  using DiskBlock = unsigned char[kBlockSize];

  void SetUp() override {
    // Set up default command expectations.
    controller_.ExpectCall(
        [this](uint8_t target, uint16_t lun, iovec cdb, bool is_write, iovec data) -> auto {
          EXPECT_EQ(target, kTarget);
          EXPECT_EQ(lun, kLun);

          switch (default_seq_) {
            case 0: {
              EXPECT_EQ(cdb.iov_len, 6);
              InquiryCDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::INQUIRY);
              EXPECT_FALSE(is_write);
              break;
            }
            case 1: {
              EXPECT_EQ(cdb.iov_len, 6);
              ModeSense6CDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::MODE_SENSE_6);
              EXPECT_EQ(decoded_cdb.page_code, ModeSense6CDB::kAllPageCode);
              EXPECT_EQ(decoded_cdb.disable_block_descriptors, 0);
              EXPECT_FALSE(is_write);
              ModeSense6ParameterHeader response = {};
              memcpy(data.iov_base, reinterpret_cast<char*>(&response), sizeof(response));
              break;
            }
            case 2: {
              EXPECT_EQ(cdb.iov_len, 6);
              ModeSense6CDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::MODE_SENSE_6);
              EXPECT_EQ(decoded_cdb.page_code, ModeSense6CDB::kCachingPageCode);
              EXPECT_EQ(decoded_cdb.disable_block_descriptors, 0b1000);
              EXPECT_FALSE(is_write);
              CachingModePage response = {};
              response.page_code = ModeSense6CDB::kCachingPageCode;
              memcpy(data.iov_base, reinterpret_cast<char*>(&response), sizeof(response));
              break;
            }
            case 3: {
              EXPECT_EQ(cdb.iov_len, 10);
              ReadCapacity10CDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::READ_CAPACITY_10);
              EXPECT_FALSE(is_write);
              ReadCapacity10ParameterData response = {};
              response.returned_logical_block_address = htobe32(UINT32_MAX);
              response.block_length_in_bytes = htobe32(kBlockSize);
              memcpy(data.iov_base, reinterpret_cast<char*>(&response), sizeof(response));
              break;
            }
            case 4: {
              EXPECT_EQ(cdb.iov_len, 16);
              ReadCapacity16CDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::READ_CAPACITY_16);
              EXPECT_EQ(decoded_cdb.service_action, 0x10);
              EXPECT_FALSE(is_write);
              ReadCapacity16ParameterData response = {};
              response.returned_logical_block_address = htobe64(kFakeBlocks - 1);
              response.block_length_in_bytes = htobe32(kBlockSize);
              memcpy(data.iov_base, reinterpret_cast<char*>(&response), sizeof(response));
              break;
            }
          }
          default_seq_++;

          return ZX_OK;
        },
        /*times=*/5);
  }

  ControllerForTest controller_;
  int default_seq_ = 0;
};

// Test that we can create a disk when the underlying controller successfully executes CDBs.
TEST_F(DiskTest, TestCreateDestroy) {
  std::shared_ptr<MockDevice> fake_parent = MockDevice::FakeRootParent();
  ASSERT_OK(Disk::Bind(fake_parent.get(), &controller_, kTarget, kLun, kTransferSize));
  ASSERT_EQ(1, fake_parent->child_count());
}

// Test creating a disk and executing read commands.
TEST_F(DiskTest, TestCreateReadDestroy) {
  std::shared_ptr<MockDevice> fake_parent = MockDevice::FakeRootParent();
  ASSERT_OK(Disk::Bind(fake_parent.get(), &controller_, kTarget, kLun, kTransferSize));
  ASSERT_EQ(1, fake_parent->child_count());
  auto* dev = fake_parent->GetLatestChild()->GetDeviceContext<Disk>();
  block_info_t info;
  size_t op_size;
  dev->BlockImplQuery(&info, &op_size);

  // To test SCSI Read functionality, create a fake "disk" backing store in memory and service
  // reads from it. Fill block 1 with a test pattern of 0x01.
  std::map<uint64_t, DiskBlock> blocks;
  DiskBlock& test_block_1 = blocks[1];
  memset(test_block_1, 0x01, sizeof(DiskBlock));

  controller_.ExpectCall(
      [&blocks](uint8_t target, uint16_t lun, iovec cdb, bool is_write, iovec data) -> auto {
        EXPECT_EQ(cdb.iov_len, 16);
        Read16CDB decoded_cdb = {};
        memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
        EXPECT_EQ(decoded_cdb.opcode, Opcode::READ_16);
        EXPECT_FALSE(is_write);

        // Support reading one block.
        EXPECT_EQ(be32toh(decoded_cdb.transfer_length), 1);
        uint64_t block_to_read = be64toh(decoded_cdb.logical_block_address);
        const DiskBlock& data_to_return = blocks.at(block_to_read);
        memcpy(data.iov_base, data_to_return, sizeof(DiskBlock));

        return ZX_OK;
      },
      /*times=*/1);

  // Issue a read to block 1 that should work.
  struct IoWait {
    fbl::Mutex lock_;
    fbl::ConditionVariable cv_;
  };
  IoWait iowait_;
  auto block_op = std::make_unique<uint8_t[]>(op_size);
  block_op_t& read = *reinterpret_cast<block_op_t*>(block_op.get());
  block_impl_queue_callback done = [](void* ctx, zx_status_t status, block_op_t* op) {
    IoWait* iowait_ = reinterpret_cast<struct IoWait*>(ctx);

    fbl::AutoLock lock(&iowait_->lock_);
    iowait_->cv_.Signal();
  };
  read.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  read.rw.length = 1;      // Read one block
  read.rw.offset_dev = 1;  // Read logical block 1
  read.rw.offset_vmo = 0;
  EXPECT_OK(zx_vmo_create(zx_system_get_page_size(), 0, &read.rw.vmo));
  controller_.AsyncIoInit();
  {
    fbl::AutoLock lock(&iowait_.lock_);
    auto* dev = fake_parent->GetLatestChild()->GetDeviceContext<Disk>();
    dev->BlockImplQueue(&read, done, &iowait_);  // NOTE: Assumes asynchronous controller
    iowait_.cv_.Wait(&iowait_.lock_);
  }
  // Make sure the contents of the VMO we read into match the expected test pattern
  DiskBlock check_buffer = {};
  EXPECT_OK(zx_vmo_read(read.rw.vmo, check_buffer, 0, sizeof(DiskBlock)));
  for (uint i = 0; i < sizeof(DiskBlock); i++) {
    EXPECT_EQ(check_buffer[i], 0x01);
  }
  controller_.AsyncIoRelease();
}

}  // namespace scsi
