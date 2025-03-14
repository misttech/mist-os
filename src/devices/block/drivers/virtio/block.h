// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_BLOCK_H_
#define SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_BLOCK_H_

#include <lib/bio.h>
#include <lib/virtio/backends/backend.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/time.h>
#include <stdlib.h>
#include <zircon/compiler.h>

#include <memory>

#include <kernel/event.h>
#include <kernel/thread.h>
#include <ktl/atomic.h>
#include <virtio/block.h>

#include "zircon/listnode.h"

namespace virtio {

// Performs a regular data read or write from the device. The operation may
// be cached internally.
#define BLOCK_OPCODE_READ 1
#define BLOCK_OPCODE_WRITE 2
// Write any controller or device cached data to nonvolatile storage.
#define BLOCK_OPCODE_FLUSH 3
// Instructs the device to invalidate a number of blocks, making them  usable
// for storing something else. This is basically a "delete" optimization,
// where the device is in charge of discarding the old content without
// clients having to write a given pattern. The operation may be cached
// internally.
#define BLOCK_OPCODE_TRIM 4
/// Detaches the VMO from the block device.
#define BLOCK_OPCODE_CLOSE_VMO 5

// Associate the following request with `group`.
#define BLOCK_IO_FLAG_GROUP_ITEM 0x00000001
// Only respond after this request (and all previous within group) have
// completed. Only valid with `GROUP_ITEM`.
#define BLOCK_IO_FLAG_GROUP_LAST 0x00000002
// Mark this operation as "Force Unit Access" (FUA), indicating that
// it should not complete until the data is written to the non-volatile
// medium (write), and that reads should bypass any on-device caches.
#define BLOCK_IO_FLAG_FORCE_ACCESS 0x00000004

struct BlockCommand {
  uint8_t opcode;
  uint32_t flags;
};

struct BlockReadWrite {
  // Opcode and flags.
  BlockCommand command;
  // Available for temporary use.
  uint32_t extra;
  // data to read or write.
  vaddr_t buf;
  // Transfer length in blocks (0 is invalid).
  uint32_t length;
  // Device offset in blocks.
  uint64_t offset_dev;
  // offset in blocks.
  uint64_t offset;
};

struct BlockTrim {
  // Opcode and flags.
  BlockCommand command;
  // Transfer length in blocks (0 is invalid).
  uint32_t length;
  // Device offset in blocks.
  uint64_t offset_dev;
};

union block_op_t {
  // All Commands
  BlockCommand command;
  // Read and Write ops use rw for parameters.
  BlockReadWrite rw;
  BlockTrim trim;
};

using block_impl_queue_callback = void (*)(void* cookie, zx_status_t status, block_op_t* op);

struct block_txn_t {
  block_op_t op;
  block_impl_queue_callback completion_cb;
  void* cookie;
  struct vring_desc* desc;
  size_t req_index;
  ktl::optional<size_t> discard_req_index;  // Only used if op is trim
  list_node_t node;
};

class Ring;
class BlockDevice : public virtio::Device, public bdev_t {
 public:
  BlockDevice(ktl::unique_ptr<Backend> backend);
  ~BlockDevice();

  // virtio::Device overrides
  zx_status_t Init() override;
  void Release() override;

  void IrqRingUpdate() override;
  void IrqConfigChange() override;

  uint32_t GetBlockSize() const { return config_.blk_size; }
  uint64_t GetBlockCount() const { return config_.capacity; }
  const char* tag() const override { return "virtio-blk"; }

  // ddk::BlockImplProtocol functions invoked by BlockDriver.
  // void BlockImplQuery(block_info_t* info, size_t* bopsz);
  void BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb, void* cookie);

 private:
  static constexpr uint16_t kRingSize = 128;  // 128 matches legacy pci.

  // A queue of block request/responses.
  static constexpr size_t kBlkReqCount = 32;

  static constexpr zx::duration kWatchdogInterval = zx::sec(10);

  void SignalWorker(block_txn_t* txn);
  void WorkerThread();
  void WatchdogThread();
  void FlushPendingTxns();
  void CleanupPendingTxns();

  zx_status_t QueueTxn(block_txn_t* txn, uint32_t type, size_t bytes, zx_paddr_t* pages,
                       size_t pagecount, uint16_t* idx);

  void txn_complete(block_txn_t* txn, zx_status_t status);

  // The main virtio ring.
  Ring vring_{this};

  // Lock to be used around Ring::AllocDescChain and FreeDesc.
  // TODO: Move this into Ring class once it's certain that other users of the class are okay with
  // it.
  DECLARE_MUTEX(BlockDevice) ring_lock_;

  // Saved block device configuration out of the pci config BAR.
  virtio_blk_config_t config_ = {};

  virtio::ContiguousBuffer blk_req_buf_;
  virtio_blk_req_t* blk_req_ = nullptr;

  zx_paddr_t blk_res_pa_ = 0;
  uint8_t* blk_res_ = nullptr;

  uint32_t blk_req_bitmap_ = 0;
  static_assert(kBlkReqCount <= sizeof(blk_req_bitmap_) * CHAR_BIT);

  // When a transaction is enqueued, its start time (in the monotonic clock) is recorded, and the
  // timestamp is cleared when the transaction completes.  A watchdog task will fire after a
  // configured interval, and all timestamps will be checked against a deadline; if any exceed the
  // deadline an error is logged.
  DECLARE_MUTEX(BlockDevice) watchdog_lock_;
  zx::time blk_req_start_timestamps_[kBlkReqCount] __TA_GUARDED(watchdog_lock_);

  size_t alloc_blk_req() {
    size_t i = 0;
    if (blk_req_bitmap_ != 0) {
      i = sizeof(blk_req_bitmap_) * CHAR_BIT - __builtin_clz(blk_req_bitmap_);
    }
    if (i < kBlkReqCount) {
      blk_req_bitmap_ |= (1 << i);
    }
    return i;
  }

  void free_blk_req(size_t i) { blk_req_bitmap_ &= ~(1 << i); }

  // Pending txns and completion signal.
  DECLARE_MUTEX(BlockDevice) txn_lock_;
  list_node pending_txn_list_ = LIST_INITIAL_VALUE(pending_txn_list_);
  Event txn_signal_;

  // Worker state.
  Thread* worker_thread_ = nullptr;
  list_node worker_txn_list_ = LIST_INITIAL_VALUE(worker_txn_list_);
  Event worker_signal_;
  ktl::atomic<bool> worker_shutdown_ = false;

  Thread* watchdog_thread_ = nullptr;
  Event watchdog_signal_;
  ktl::atomic<bool> watchdog_shutdown_ = false;

  bool supports_discard_ = false;
};

}  // namespace virtio

#endif  // SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_BLOCK_H_
