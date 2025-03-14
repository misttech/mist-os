// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "block.h"

#include <inttypes.h>
#include <lib/bio.h>
#include <lib/fit/defer.h>
#include <lib/zircon-internal/align.h>
#include <lib/zx/time.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <trace.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <cstdint>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <fbl/inline_array.h>
#include <kernel/lockdep.h>
#include <pretty/hexdump.h>

#include "src/devices/block/lib/common/include/common.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

// 1MB max transfer (unless further restricted by ring size).
#define MAX_SCATTER 257

namespace virtio {

// Cache some page size calculations that are used frequently.
static const uint32_t kPageSize = PAGE_SIZE;
static const uint32_t kPageMask = kPageSize - 1;
// static const uint32_t kMaxMaxXfer = (MAX_SCATTER - 1) * kPageSize;

void BlockDevice::txn_complete(block_txn_t* txn, zx_status_t status) {
  {
    Guard<Mutex> guard(&watchdog_lock_);
    blk_req_start_timestamps_[txn->req_index] = zx::time::infinite();
  }
  txn->completion_cb(txn->cookie, status, &txn->op);
}

#if 0
void BlockDevice::BlockImplQuery(block_info_t* info, size_t* bopsz) {
  memset(info, 0, sizeof(*info));
  info->block_size = GetBlockSize();
  info->block_count = GetBlockCount();
  info->max_transfer_size = static_cast<uint32_t>(kPageSize * (kRingSize - 2));
  if (supports_discard_) {
    info->flags |= BLOCK_FLAG_TRIM_SUPPORT;
  }

  // Limit max transfer to our worst case scatter list size.
  if (info->max_transfer_size > kMaxMaxXfer) {
    info->max_transfer_size = kMaxMaxXfer;
  }
  *bopsz = sizeof(block_txn_t);
}
#endif

void BlockDevice::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                 void* cookie) {
  block_txn_t* txn = static_cast<block_txn_t*>((void*)bop);
  txn->completion_cb = completion_cb;
  txn->cookie = cookie;
  SignalWorker(txn);
}

BlockDevice::BlockDevice(ktl::unique_ptr<Backend> backend) : virtio::Device(ktl::move(backend)) {
  txn_signal_.Unsignal();
  worker_signal_.Unsignal();
  watchdog_signal_.Unsignal();
  for (auto& time : blk_req_start_timestamps_) {
    time = zx::time::infinite();
  }
}

BlockDevice::~BlockDevice() { LTRACE_EXIT_OBJ; }

zx_status_t BlockDevice::Init() {
  DeviceReset();
  CopyDeviceConfig(&config_, sizeof(config_));

  // TODO(cja): The blk_size provided in the device configuration is only
  // populated if a specific feature bit has been negotiated during
  // initialization, otherwise it is 0, at least in Virtio 0.9.5. Use 512
  // as a default as a stopgap for now until proper feature negotiation
  // is supported.
  if (config_.blk_size == 0) {
    config_.blk_size = 512;
  }

  LTRACEF("capacity %#" PRIx64 "\n", config_.capacity);
  LTRACEF("size_max %#x\n", config_.size_max);
  LTRACEF("seg_max  %#x\n", config_.seg_max);
  LTRACEF("blk_size %#x\n", config_.blk_size);

  DriverStatusAck();

  uint64_t features = DeviceFeaturesSupported();
  if (!(features & VIRTIO_F_VERSION_1)) {
    // Declaring non-support until there is a need in the future.
    dprintf(CRITICAL, "Legacy virtio interface is not supported by this driver\n");
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (features & VIRTIO_BLK_F_DISCARD) {
    dprintf(SPEW, "virtio device supports discard\n");
    supports_discard_ = true;
  }
  features &= (VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_DISCARD);
  DriverFeaturesAck(features);
  if (zx_status_t status = DeviceStatusFeaturesOk(); status != ZX_OK) {
    dprintf(CRITICAL, "Feature negotiation failed: %d\n", status);
    return status;
  }

  // Allocate the main vring.
  auto err = vring_.Init(0, kRingSize);
  if (err < 0) {
    dprintf(CRITICAL, "Failed to allocate vring: %d\n", err);
    return err;
  }

  // Allocate a queue of block requests.
  size_t size = sizeof(virtio_blk_req_t) * kBlkReqCount + sizeof(uint8_t) * kBlkReqCount;

  zx_status_t status = VmAspace::kernel_aspace()->AllocContiguous(
      "blk_req_buf", size, &blk_req_buf_.ptr, 0, VmAspace::VMM_FLAG_COMMIT,
      ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE);
  if (status != ZX_OK) {
    dprintf(CRITICAL, "cannot alloc blk_req buffers: %d\n", status);
    return status;
  }
  blk_req_buf_.pa = vaddr_to_paddr(blk_req_buf_.ptr);
  blk_req_ = static_cast<virtio_blk_req_t*>(blk_req_buf_.ptr);

  LTRACEF("allocated blk request at %p, physical address %#" PRIxPTR "\n", blk_req_,
          blk_req_buf_.pa);

  // Responses are 32 words at the end of the allocated block.
  blk_res_pa_ = blk_req_buf_.pa + sizeof(virtio_blk_req_t) * kBlkReqCount;
  blk_res_ = reinterpret_cast<uint8_t*>(
      (reinterpret_cast<uintptr_t>(blk_req_) + sizeof(virtio_blk_req_t) * kBlkReqCount));

  LTRACEF("allocated blk responses at %p, physical address %#" PRIxPTR "\n", blk_res_, blk_res_pa_);

  StartIrqThread();
  DriverStatusOk();

  auto worker_thread_entry = [](void* ctx) {
    auto bd = static_cast<BlockDevice*>(ctx);
    bd->WorkerThread();
    return ZX_OK;
  };
  worker_thread_ =
      Thread::Create("virtio-block-worker", worker_thread_entry, this, DEFAULT_PRIORITY);
  if (worker_thread_ == nullptr) {
    return ZX_ERR_INTERNAL;
  }
  worker_thread_->Resume();

  auto watchdog_thread_entry = [](void* ctx) {
    auto bd = static_cast<BlockDevice*>(ctx);
    bd->WatchdogThread();
    return ZX_OK;
  };
  watchdog_thread_ =
      Thread::Create("virtio-block-watchdog", watchdog_thread_entry, this, DEFAULT_PRIORITY);
  if (watchdog_thread_ == nullptr) {
    return ZX_ERR_INTERNAL;
  }
  watchdog_thread_->Resume();

  // construct the block device
  static uint8_t found_index = 0;
  ktl::array<char, ZX_MAX_NAME_LEN> block_name{};
  snprintf(block_name.data(), block_name.size(), "virtio%u", found_index++);
  bio_initialize_bdev(this, block_name.data(), config_.blk_size,
                      static_cast<bnum_t>(config_.capacity), 0, nullptr, BIO_FLAGS_NONE);

  /* override our block device hooks */
  auto read_block = [](struct bdev* dev, void* buf, bnum_t block, uint count) -> ssize_t {
    LTRACEF("dev %p, buf %p, block %d, count %d\n", dev, buf, block, count);
    Event read_complete;
    auto thiz = static_cast<BlockDevice*>(dev);
    auto length = count;
    block_txn_t txn;
    memset(&txn, 0, sizeof(txn));
    txn.op.rw.command.opcode = BLOCK_OPCODE_READ;
    txn.op.rw.length = static_cast<uint32_t>(length);
    txn.op.rw.vaddr = (vaddr_t)buf;
    txn.op.rw.offset_dev = block;

    thiz->BlockImplQueue(
        reinterpret_cast<block_op_t*>(&txn),
        [](void* cookie, zx_status_t status, block_op_t* op) {
          auto read_complete = static_cast<Event*>(cookie);
          read_complete->Signal(status);
        },
        &read_complete);

    auto result = read_complete.Wait();
    if (result != ZX_OK) {
      return static_cast<ssize_t>(result);
    }
    return count * dev->block_size;
  };

  auto write_block = [](struct bdev* dev, const void* buf, bnum_t block, uint count) -> ssize_t {
    LTRACEF("dev %p, buf %p, block %d, count %d\n", dev, buf, block, count);
    return 0;
  };

  this->read_block = read_block;
  this->write_block = write_block;

  bio_register_device(this);

  return ZX_OK;
}

void BlockDevice::Release() {
  watchdog_shutdown_.store(true);
  watchdog_signal_.Signal();
  worker_shutdown_.store(true);
  worker_signal_.Signal();
  txn_signal_.Signal();

  watchdog_thread_->Join(nullptr, ZX_TIME_INFINITE);
  worker_thread_->Join(nullptr, ZX_TIME_INFINITE);
  virtio::Device::Release();
}

void BlockDevice::IrqRingUpdate() {
  // Parse our descriptor chain and add back to the free queue.
  auto free_chain = [this](vring_used_elem* used_elem) {
    uint32_t i = static_cast<uint16_t>(used_elem->id);
    struct vring_desc* desc = vring_.DescFromIndex(static_cast<uint16_t>(i));
    auto head_desc = desc;  // Save the first element.
    {
      Guard<Mutex> guard(&ring_lock_);
      for (;;) {
        int next;
        if (LOCAL_TRACE >= 2) {
          virtio_dump_desc(desc);
        }
        if (desc->flags & VRING_DESC_F_NEXT) {
          next = desc->next;
        } else {
          // End of chain.
          next = -1;
        }

        vring_.FreeDesc(static_cast<uint16_t>(i));

        if (next < 0)
          break;
        i = next;
        desc = vring_.DescFromIndex(static_cast<uint16_t>(i));
      }
    }

    ktl::optional<uint8_t> status;
    block_txn_t* txn = nullptr;
    {
      Guard<Mutex> guard(&txn_lock_);

      // Search our pending txn list to see if this completes it.
      list_for_every_entry (&pending_txn_list_, txn, block_txn_t, node) {
        if (txn->desc == head_desc) {
          LTRACEF("completes txn %p\n", txn);
          status = blk_res_[txn->req_index];

          free_blk_req(txn->req_index);
          if (txn->discard_req_index) {
            free_blk_req(*txn->discard_req_index);
          }
          list_delete(&txn->node);

          txn_signal_.Signal();
          break;
        }
      }
    }

    if (status) {
      zx_status_t zx_status = ZX_ERR_IO;
      switch (*status) {
        case VIRTIO_BLK_S_OK:
          zx_status = ZX_OK;
          break;
        case VIRTIO_BLK_S_IOERR:
          break;
        case VIRTIO_BLK_S_UNSUPP:
          zx_status = ZX_ERR_NOT_SUPPORTED;
      }
      txn_complete(txn, zx_status);
    }
  };

  // Tell the ring to find free chains and hand it back to our lambda.
  vring_.IrqRingUpdate(free_chain);
}

void BlockDevice::IrqConfigChange() {}

zx_status_t BlockDevice::QueueTxn(block_txn_t* txn, uint32_t type, size_t bytes, zx_paddr_t* pages,
                                  size_t pagecount, uint16_t* idx) {
  size_t req_index;
  ktl::optional<size_t> discard_req_index;
  {
    Guard<Mutex> guard(&txn_lock_);
    req_index = alloc_blk_req();
    if (req_index >= kBlkReqCount) {
      LTRACEF("too many block requests queued (%zu)!\n", req_index);
      return ZX_ERR_NO_RESOURCES;
    }
    if (type == VIRTIO_BLK_T_DISCARD) {
      // A second descriptor needs to be allocated for discard requests.
      discard_req_index = alloc_blk_req();
      if (*discard_req_index >= kBlkReqCount) {
        LTRACEF("too many block requests queued (%zu)!\n", *discard_req_index);
        free_blk_req(req_index);
        return ZX_ERR_NO_RESOURCES;
      }
    }
  }

  auto req = &blk_req_[req_index];
  req->type = type;
  req->ioprio = 0;
  if (type == VIRTIO_BLK_T_FLUSH) {
    req->sector = 0;
  } else {
    req->sector = txn->op.rw.offset_dev;
  }
  LTRACEF("blk_req type %u ioprio %u sector %" PRIu64 "\n", req->type, req->ioprio, req->sector);

  if (type == VIRTIO_BLK_T_DISCARD) {
    // NOTE: if we decide to later send multiple virtio_blk_discard_write_zeroes at once, we must
    // respect the max_discard_seg configuration of the device.
    static_assert(sizeof(virtio_blk_discard_write_zeroes_t) <= sizeof(virtio_blk_req_t));
    virtio_blk_discard_write_zeroes_t* request =
        reinterpret_cast<virtio_blk_discard_write_zeroes_t*>(&blk_req_[req_index]);
    request->sector = txn->op.rw.offset_dev;
    request->num_sectors = txn->op.rw.length;
    LTRACEF("blk_dwz_req sector %" PRIu64 " num_sectors %" PRIu32 "\n", request->sector,
            request->num_sectors);
  }
  // Save the request indexes so we can free them when we complete the transfer.
  txn->req_index = req_index;
  txn->discard_req_index = discard_req_index;

  LTRACEF("page count %lu\n", pagecount);

  // Put together a transfer.
  uint16_t i;
  vring_desc* desc;
  uint16_t num_descriptors =
      (type == VIRTIO_BLK_T_DISCARD ? 3u : 2u) + static_cast<uint16_t>(pagecount);
  {
    Guard<Mutex> guard(&ring_lock_);
    desc = vring_.AllocDescChain(num_descriptors, &i);
  }
  if (!desc) {
    dprintf(CRITICAL, "failed to allocate descriptor chain of length %zu", 2u + pagecount);
    Guard<Mutex> guard(&txn_lock_);
    free_blk_req(req_index);
    if (discard_req_index) {
      free_blk_req(*discard_req_index);
    }
    return ZX_ERR_NO_RESOURCES;
  }

  LTRACEF("after alloc chain desc %p, i %u\n", desc, i);

  // Point the txn at this head descriptor.
  txn->desc = desc;

  // Set up the descriptor pointing to the head.
  desc->addr = blk_req_buf_.pa + req_index * sizeof(virtio_blk_req_t);
  desc->len = sizeof(virtio_blk_req_t);
  desc->flags = VRING_DESC_F_NEXT;
  if (LOCAL_TRACE >= 2) {
    virtio_dump_desc(desc);
  }

  for (size_t n = 0; n < pagecount; n++) {
    desc = vring_.DescFromIndex(desc->next);
    desc->addr = pages[n];  // |pages| are all page-aligned addresses.
    desc->len = static_cast<uint32_t>((bytes > kPageSize) ? kPageSize : bytes);
    if (n == 0) {
      // First entry may not be page aligned.
      size_t page0_offset = txn->op.rw.offset & kPageMask;

      // Adjust starting address.
      desc->addr += page0_offset;

      // Trim length if necessary.
      size_t max = kPageSize - page0_offset;
      if (desc->len > max) {
        desc->len = static_cast<uint32_t>(max);
      }
    }
    desc->flags = VRING_DESC_F_NEXT;
    LTRACEF("pa %#lx, len %#x\n", desc->addr, desc->len);

    // Mark buffer as write-only if its a block read.
    if (type == VIRTIO_BLK_T_IN) {
      desc->flags |= VRING_DESC_F_WRITE;
    }

    bytes -= desc->len;
  }
  if (LOCAL_TRACE >= 2) {
    virtio_dump_desc(desc);
  }
  assert(bytes == 0);

  if (type == VIRTIO_BLK_T_DISCARD) {
    desc = vring_.DescFromIndex(desc->next);
    desc->addr = blk_req_buf_.pa + *discard_req_index * sizeof(virtio_blk_req_t);
    desc->len = sizeof(virtio_blk_discard_write_zeroes_t);
    desc->flags = VRING_DESC_F_NEXT;
    if (LOCAL_TRACE >= 2) {
      virtio_dump_desc(desc);
    }
  }

  // Set up the descriptor pointing to the response.
  desc = vring_.DescFromIndex(desc->next);
  desc->addr = blk_res_pa_ + req_index;
  desc->len = 1;
  desc->flags = VRING_DESC_F_WRITE;
  if (LOCAL_TRACE >= 2) {
    virtio_dump_desc(desc);
  }

  {
    Guard<Mutex> guard(&watchdog_lock_);
    blk_req_start_timestamps_[req_index] = zx::time(current_mono_time());
  }

  *idx = i;
  return ZX_OK;
}

namespace {

// Type used to refer to virtual addresses presented to a device by the IOMMU.
typedef uint64_t dev_vaddr_t;

// Out parameter |pages| are all page-aligned addresses.
static zx_status_t pin_pages(block_txn_t* txn, size_t bytes, zx_paddr_t* pages, size_t* num_pages) {
#if 0
  uint64_t suboffset = txn->op.rw.offset & kPageMask;
  uint64_t aligned_offset = txn->op.rw.offset & ~kPageMask;
  size_t pin_size = ZX_ROUNDUP(suboffset + bytes, kPageSize);
  *num_pages = pin_size / kPageSize;
  if (*num_pages > MAX_SCATTER) {
    LTRACEF("transaction too large\n");
    return ZX_ERR_INVALID_ARGS;
  };

  // Address count is currently limited to the amount of addresses that can fit on 64 pages. This
  // is large enough for all current usage of bti_pin, but protects against the case of an
  // arbitrarily large array being allocated on the heap.
  constexpr size_t kMaxAddrs = (PAGE_SIZE * 64) / sizeof(dev_vaddr_t);
  if (!IS_PAGE_ALIGNED(aligned_offset) || !IS_PAGE_ALIGNED(pin_size) || *num_pages > kMaxAddrs) {
    return ZX_ERR_INVALID_ARGS;
  }

  constexpr size_t kAddrsLenLimitForStack = 4;
  fbl::AllocChecker ac;
  fbl::InlineArray<dev_vaddr_t, kAddrsLenLimitForStack> mapped_addrs(&ac, *num_pages);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  DEBUG_ASSERT(IS_PAGE_ALIGNED(aligned_offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(pin_size));

  if (pin_size == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (*num_pages == 1)

    /*zx_handle_t vmo = txn->op.rw.vmo;
    zx_status_t status = zx_bti_pin(bti, ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, vmo, aligned_offset,
                                    pin_size, pages, *num_pages, &txn->pmt);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "could not pin pages: %s", zx_status_get_string(status));
      return ZX_ERR_INTERNAL;
    }*/
#endif
  vaddr_t va = (vaddr_t)txn->op.rw.vaddr;
  pages[0] = vaddr_to_paddr((void*)va);
  *num_pages = 1;

  return ZX_OK;
}
}  // namespace

void BlockDevice::SignalWorker(block_txn_t* txn) {
  switch (txn->op.command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE:
    case BLOCK_OPCODE_TRIM:
      if (zx_status_t status = block::CheckIoRange(txn->op.rw, config_.capacity); status != ZX_OK) {
        txn_complete(txn, status);
        return;
      }
      if (txn->op.command.flags & BLOCK_IO_FLAG_FORCE_ACCESS) {
        txn_complete(txn, ZX_ERR_NOT_SUPPORTED);
        return;
      }
      LTRACEF("txn %p, opcode %#x\n", txn, txn->op.command.opcode);
      break;
    case BLOCK_OPCODE_FLUSH:
      LTRACEF("txn %p, opcode FLUSH\n", txn);
      break;
    default:
      txn_complete(txn, ZX_ERR_NOT_SUPPORTED);
      return;
  }

  Guard<Mutex> guard(&lock_);
  if (worker_shutdown_.load()) {
    txn_complete(txn, ZX_ERR_IO_NOT_PRESENT);
    return;
  }
  list_add_tail(&worker_txn_list_, &txn->node);
  worker_signal_.Signal();
}

void BlockDevice::WorkerThread() {
  auto cleanup = fit::defer([this]() { CleanupPendingTxns(); });
  block_txn_t* txn = nullptr;
  for (;;) {
    if (worker_shutdown_.load()) {
      return;
    }

    // Pull a txn off the list or wait to be signaled.
    {
      Guard<Mutex> guard(&lock_);
      txn = list_remove_head_type(&worker_txn_list_, block_txn_t, node);
    }
    if (!txn) {
      worker_signal_.Wait(Deadline::infinite());
      worker_signal_.Unsignal();
      continue;
    }

    LTRACEF("WorkerThread handling txn %p\n", txn);

    uint32_t type;
    bool do_flush = false;
    size_t bytes;
    zx_paddr_t pages[MAX_SCATTER];
    size_t num_pages;
    zx_status_t status = ZX_OK;

    if (txn->op.command.opcode == BLOCK_OPCODE_FLUSH) {
      type = VIRTIO_BLK_T_FLUSH;
      bytes = 0;
      num_pages = 0;
      do_flush = true;
    } else if (txn->op.command.opcode == BLOCK_OPCODE_TRIM) {
      type = VIRTIO_BLK_T_DISCARD;
      bytes = 0;
      num_pages = 0;
    } else {
      if (txn->op.command.opcode == BLOCK_OPCODE_WRITE) {
        type = VIRTIO_BLK_T_OUT;
      } else {
        type = VIRTIO_BLK_T_IN;
      }
      bytes = static_cast<size_t>(txn->op.rw.length) * config_.blk_size;
      txn->op.rw.offset *= config_.blk_size;
      status = pin_pages(txn, bytes, pages, &num_pages);
    }

    if (status != ZX_OK) {
      txn_complete(txn, status);
      continue;
    }

    // A flush operation should complete after any inflight transactions, so wait for all
    // pending txns to complete before submitting a flush txn. This is necessary because
    // a virtio block device may service requests in any order.
    if (do_flush) {
      FlushPendingTxns();
      if (worker_shutdown_.load()) {
        return;
      }
    }

    bool cannot_fail = false;
    for (;;) {
      uint16_t idx;
      status = QueueTxn(txn, type, bytes, pages, num_pages, &idx);
      if (status == ZX_OK) {
        Guard<Mutex> guard(&txn_lock_);
        list_add_tail(&pending_txn_list_, &txn->node);
        vring_.SubmitChain(idx);
        vring_.Kick();
        LTRACEF("WorkerThread submitted txn %p\n", txn);
        break;
      }

      if (cannot_fail) {
        LTRACEF("failed to queue txn to hw: %d\n", status);
        {
          Guard<Mutex> guard(&txn_lock_);
          free_blk_req(txn->req_index);
          if (txn->discard_req_index) {
            free_blk_req(*txn->discard_req_index);
          }
        }
        txn_complete(txn, status);
        break;
      }

      {
        Guard<Mutex> guard(&txn_lock_);
        if (list_is_empty(&pending_txn_list_)) {
          // We hold the txn lock and the list is empty, if we fail this time around
          // there's no point in trying again.
          cannot_fail = true;
          continue;
        }

        // Reset the txn signal then wait for one of the pending txns to complete
        // outside the lock. This should mean that resources have been freed for the next
        // iteration. We cannot deadlock due to the reset because pending_txn_list_ is not
        // empty.
        txn_signal_.Unsignal();
      }

      txn_signal_.Wait(Deadline::infinite());
      if (worker_shutdown_.load()) {
        return;
      }
    }

    // A flush operation should complete before any subsequent transactions. So, we wait for all
    // pending transactions (including the flush) to complete before continuing.
    if (do_flush) {
      FlushPendingTxns();
    }
  }
}

void BlockDevice::WatchdogThread() {
  for (;;) {
    watchdog_signal_.Wait(Deadline::after_mono(kWatchdogInterval.to_secs()));
    if (watchdog_shutdown_.load()) {
      return;
    }
    zx::time now = zx::time(current_mono_time());
    {
      Guard<Mutex> guard(&watchdog_lock_);
      int idx = 0;
      for (const auto& start_time : blk_req_start_timestamps_) {
        if (now - kWatchdogInterval >= start_time) {
          // Round down to the interval
          zx::duration latency = ((now - start_time) / kWatchdogInterval) * kWatchdogInterval;
          LTRACEF("txn %d has not completed after %" PRIu64 "s!\n", idx, latency.to_secs());
        }
        idx += 1;
      }
    }
  }
}

void BlockDevice::FlushPendingTxns() {
  for (;;) {
    {
      Guard<Mutex> guard(&txn_lock_);
      if (list_is_empty(&pending_txn_list_)) {
        return;
      }
      txn_signal_.Unsignal();
    }
    txn_signal_.Wait(Deadline::infinite());
    if (worker_shutdown_.load()) {
      return;
    }
  }
}

void BlockDevice::CleanupPendingTxns() {
  // Virtio specification 3.3.1 Driver Requirements: Device Cleanup
  // A driver MUST ensure a virtqueue isn’t live (by device reset) before removing exposed
  // buffers.
  DeviceReset();
  block_txn_t* txn = nullptr;
  block_txn_t* temp_entry = nullptr;
  {
    Guard<Mutex> guard(&lock_);
    list_for_every_entry_safe (&worker_txn_list_, txn, temp_entry, block_txn_t, node) {
      list_delete(&txn->node);
      txn_complete(txn, ZX_ERR_IO_NOT_PRESENT);
    }
  }
  Guard<Mutex> guard(&txn_lock_);
  list_for_every_entry_safe (&pending_txn_list_, txn, temp_entry, block_txn_t, node) {
    free_blk_req(txn->req_index);
    if (txn->discard_req_index) {
      free_blk_req(*txn->discard_req_index);
    }
    list_delete(&txn->node);
    txn_complete(txn, ZX_ERR_IO_NOT_PRESENT);
  }
}

}  // namespace virtio
