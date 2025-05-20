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
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/time.h>
#include <mistos/hardware/block/driver/c/banjo.h>
#include <stdlib.h>
#include <zircon/compiler.h>

#include <memory>
#include <span>

#include <kernel/event.h>
#include <kernel/thread.h>
#include <ktl/atomic.h>
#include <virtio/block.h>

#include "zircon/listnode.h"

namespace virtio {

// 1MB max transfer (unless further restricted by ring size).
#define MAX_SCATTER 257

struct block_txn_t {
  block_op_t op;
  // Only set for requests coming from the block server
  //std::optional<block_server::RequestId> request;
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
  BlockDevice(fbl::RefPtr<BusTransactionInitiatorDispatcher> bti, ktl::unique_ptr<Backend> backend);
  ~BlockDevice();

  // virtio::Device overrides
  zx_status_t Init() override;
  void Release() override;

  void IrqRingUpdate() override;
  void IrqConfigChange() override;

  uint32_t GetBlockSize() const { return config_.blk_size; }
  uint64_t GetBlockCount() const { return config_.capacity; }
  uint32_t GetMaxTransferSize() const;
  flag_t GetFlags() const;
  const char* tag() const override { return "virtio-blk"; }

  // ddk::BlockImplProtocol functions invoked by BlockDriver.
  // void BlockImplQuery(block_info_t* info, size_t* bopsz);
  void BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb, void* cookie);

  // block_server::Interface
  //void StartThread(block_server::Thread) override;
  //void OnNewSession(block_server::Session) override;
  //void OnRequests(std::span<block_server::Request>) override;
  //void Log(std::string_view msg) const override {
  //  FDF_LOGL(INFO, logger(), "%.*s", static_cast<int>(msg.size()), msg.data());
  //}

  //void ServeRequests(fidl::ServerEnd<fuchsia_hardware_block_volume::Volume>);

 private:
  static constexpr uint16_t kRingSize = 128;  // 128 matches legacy pci.

  // A queue of block request/responses.
  static constexpr size_t kBlkReqCount = 32;

  static constexpr zx::duration kWatchdogInterval = zx::sec(10);

  // An object which bundles all of the resources needed to enqueue a transaction.
  // Once a transaction is successfully enqueued, RequestContext::Release() must be called, since
  // the resources are logically owned by the transaction.
  // If the RequestContext needs to be prematurely destroyed, BlockDevice::FreeResources() must be
  // called.
  class RequestContext {
   public:
    RequestContext() = default;
    RequestContext(RequestContext&&);
    RequestContext& operator=(RequestContext&&);
    RequestContext(const RequestContext&) = delete;
    RequestContext& operator=(const RequestContext&) = delete;
    ~RequestContext();

    // Explicitly acknowledge that the resources have been released by enqueueing a transaction.
    void Release();

    void set_req_index(size_t value) { req_index_ = value; }
    void set_discard_req_index(size_t value) { discard_req_index_ = value; }
    void set_desc(struct vring_desc* desc, uint16_t desc_index) {
      desc_ = desc;
      desc_index_ = desc_index;
    }
    void set_pages(zx_paddr_t* pages, size_t num_pages) {
      pages_ = pages;
      num_pages_ = num_pages;
    }
    void set_pmt(zx_handle_t value) { pmt_ = value; }

    size_t req_index() const { return req_index_; }
    const std::optional<size_t>& discard_req_index() const { return discard_req_index_; }
    uint16_t desc_index() const { return desc_index_; }
    struct vring_desc* desc() { return desc_; }
    zx_paddr_t* pages() { return pages_; }
    size_t num_pages() const { return num_pages_; }
    zx_handle_t pmt() const { return pmt_; }

   private:
    bool released_ = false;
    size_t req_index_ = kBlkReqCount;
    std::optional<size_t> discard_req_index_ = std::nullopt;
    uint16_t desc_index_ = UINT16_MAX;
    struct vring_desc* desc_ = nullptr;
    zx_paddr_t* pages_ = nullptr;
    size_t num_pages_ = 0;
    zx_handle_t pmt_ = ZX_HANDLE_INVALID;
  };

  void FreeRequestContext(RequestContext& context) __TA_REQUIRES(txn_lock_, ring_lock_);

  // Submit a transaction to the worker queue.  Only to be called for Banjo requests.
  void SignalWorker(block_txn_t*);
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
  // Lock ordering: Must be locked *after* txn_lock_.
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

  void free_blk_req(size_t i) { blk_req_bitmap_ &= static_cast<uint32_t>(~(1 << i)); }

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
