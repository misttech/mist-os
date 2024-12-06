// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_BLOCK_H_
#define SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_BLOCK_H_

#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/dma-buffer/buffer.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/sync/completion.h>
#include <lib/virtio/backends/backend.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/time.h>
#include <stdlib.h>
#include <zircon/compiler.h>

#include <atomic>
#include <memory>

#include <virtio/block.h>

#include "src/lib/listnode/listnode.h"

namespace virtio {

struct block_txn_t {
  block_op_t op;
  block_impl_queue_callback completion_cb;
  void* cookie;
  struct vring_desc* desc;
  size_t req_index;
  std::optional<size_t> discard_req_index;  // Only used if op is trim
  list_node_t node;
  zx_handle_t pmt;
};

class Ring;
class BlockDriver;
class BlockDevice : public virtio::Device {
 public:
  BlockDevice(zx::bti bti, std::unique_ptr<Backend> backend, fdf::Logger& logger);

  // virtio::Device overrides
  zx_status_t Init() override;
  void Release() override;

  void IrqRingUpdate() override;
  void IrqConfigChange() override;

  uint32_t GetBlockSize() const { return config_.blk_size; }
  uint64_t GetBlockCount() const { return config_.capacity; }
  const char* tag() const override { return "virtio-blk"; }

  // ddk::BlockImplProtocol functions invoked by BlockDriver.
  void BlockImplQuery(block_info_t* info, size_t* bopsz);
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

  fdf::Logger& logger() { return logger_; }

  // The main virtio ring.
  Ring vring_{this};

  // Lock to be used around Ring::AllocDescChain and FreeDesc.
  // TODO: Move this into Ring class once it's certain that other users of the class are okay with
  // it.
  std::mutex ring_lock_;

  // Saved block device configuration out of the pci config BAR.
  virtio_blk_config_t config_ = {};

  std::unique_ptr<dma_buffer::ContiguousBuffer> blk_req_buf_;
  virtio_blk_req_t* blk_req_ = nullptr;

  zx_paddr_t blk_res_pa_ = 0;
  uint8_t* blk_res_ = nullptr;

  uint32_t blk_req_bitmap_ = 0;
  static_assert(kBlkReqCount <= sizeof(blk_req_bitmap_) * CHAR_BIT, "");

  // When a transaction is enqueued, its start time (in the monotonic clock) is recorded, and the
  // timestamp is cleared when the transaction completes.  A watchdog task will fire after a
  // configured interval, and all timestamps will be checked against a deadline; if any exceed the
  // deadline an error is logged.
  std::mutex watchdog_lock_;
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
  std::mutex txn_lock_;
  list_node pending_txn_list_ = LIST_INITIAL_VALUE(pending_txn_list_);
  sync_completion_t txn_signal_;

  // Worker state.
  thrd_t worker_thread_;
  list_node worker_txn_list_ = LIST_INITIAL_VALUE(worker_txn_list_);
  sync_completion_t worker_signal_;
  std::atomic_bool worker_shutdown_ = false;

  thrd_t watchdog_thread_;
  sync_completion_t watchdog_signal_;
  std::atomic_bool watchdog_shutdown_ = false;

  bool supports_discard_ = false;

  fdf::Logger& logger_;
};

class BlockDriver : public fdf::DriverBase, public ddk::BlockImplProtocol<BlockDriver> {
 public:
  static constexpr char kDriverName[] = "virtio-block";

  BlockDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)) {}

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // ddk::BlockImplProtocol functions passed through to BlockDevice.
  void BlockImplQuery(block_info_t* info, size_t* bopsz);
  void BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb, void* cookie);

 protected:
  // Override to inject dependency for unit testing.
  virtual zx::result<std::unique_ptr<BlockDevice>> CreateBlockDevice();

 private:
  std::unique_ptr<BlockDevice> block_device_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;

  compat::BanjoServer block_impl_server_{ZX_PROTOCOL_BLOCK_IMPL, this, &block_impl_protocol_ops_};
  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace virtio

#endif  // SRC_DEVICES_BLOCK_DRIVERS_VIRTIO_BLOCK_H_
