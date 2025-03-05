// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "block.h"

#include <fuchsia/hardware/block/c/banjo.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <inttypes.h>
#include <lib/fit/defer.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zircon-internal/align.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <cstdint>
#include <memory>
#include <mutex>
#include <utility>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <pretty/hexdump.h>

#include "src/devices/block/lib/common/include/common.h"

#define LOCAL_TRACE 0

// 1MB max transfer (unless further restricted by ring size).
#define MAX_SCATTER 257

namespace virtio {

// Cache some page size calculations that are used frequently.
static const uint32_t kPageSize = zx_system_get_page_size();
static const uint32_t kPageMask = kPageSize - 1;
static const uint32_t kMaxMaxXfer = (MAX_SCATTER - 1) * kPageSize;

void BlockDevice::txn_complete(block_txn_t* txn, zx_status_t status) {
  if (txn->pmt != ZX_HANDLE_INVALID) {
    zx_pmt_unpin(txn->pmt);
    txn->pmt = ZX_HANDLE_INVALID;
  }
  {
    std::lock_guard<std::mutex> lock(watchdog_lock_);
    blk_req_start_timestamps_[txn->req_index] = zx::time::infinite();
  }
  txn->completion_cb(txn->cookie, status, &txn->op);
}

void BlockDevice::BlockImplQuery(block_info_t* info, size_t* bopsz) {
  memset(info, 0, sizeof(*info));
  info->block_size = GetBlockSize();
  info->block_count = GetBlockCount();
  info->max_transfer_size = static_cast<uint32_t>(kPageSize * (kRingSize - 2));
  if (supports_discard_) {
    info->flags |= FLAG_TRIM_SUPPORT;
  }

  // Limit max transfer to our worst case scatter list size.
  if (info->max_transfer_size > kMaxMaxXfer) {
    info->max_transfer_size = kMaxMaxXfer;
  }
  *bopsz = sizeof(block_txn_t);
}

void BlockDevice::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                 void* cookie) {
  block_txn_t* txn = static_cast<block_txn_t*>((void*)bop);
  txn->pmt = ZX_HANDLE_INVALID;
  txn->completion_cb = completion_cb;
  txn->cookie = cookie;
  SignalWorker(txn);
}

BlockDevice::BlockDevice(zx::bti bti, std::unique_ptr<Backend> backend, fdf::Logger& logger)
    : virtio::Device(std::move(bti), std::move(backend)), logger_(logger) {
  sync_completion_reset(&txn_signal_);
  sync_completion_reset(&worker_signal_);
  for (auto& time : blk_req_start_timestamps_) {
    time = zx::time::infinite();
  }
}

zx_status_t BlockDevice::Init() {
  DeviceReset();
  CopyDeviceConfig(&config_, sizeof(config_));

  // TODO(cja): The blk_size provided in the device configuration is only
  // populated if a specific feature bit has been negotiated during
  // initialization, otherwise it is 0, at least in Virtio 0.9.5. Use 512
  // as a default as a stopgap for now until proper feature negotiation
  // is supported.
  if (config_.blk_size == 0)
    config_.blk_size = 512;

  FDF_LOG(DEBUG, "capacity %#" PRIx64 "", config_.capacity);
  FDF_LOG(DEBUG, "size_max %#x", config_.size_max);
  FDF_LOG(DEBUG, "seg_max  %#x", config_.seg_max);
  FDF_LOG(DEBUG, "blk_size %#x", config_.blk_size);

  DriverStatusAck();

  uint64_t features = DeviceFeaturesSupported();
  if (!(features & VIRTIO_F_VERSION_1)) {
    // Declaring non-support until there is a need in the future.
    FDF_LOG(ERROR, "Legacy virtio interface is not supported by this driver");
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (features & VIRTIO_BLK_F_DISCARD) {
    FDF_LOG(INFO, "virtio device supports discard");
    supports_discard_ = true;
  }
  features &= (VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_DISCARD);
  DriverFeaturesAck(features);
  if (zx_status_t status = DeviceStatusFeaturesOk(); status != ZX_OK) {
    FDF_LOG(ERROR, "Feature negotiation failed: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate the main vring.
  auto err = vring_.Init(0, kRingSize);
  if (err < 0) {
    FDF_LOG(ERROR, "failed to allocate vring");
    return err;
  }

  // Allocate a queue of block requests.
  size_t size = sizeof(virtio_blk_req_t) * kBlkReqCount + sizeof(uint8_t) * kBlkReqCount;

  auto buffer_factory = dma_buffer::CreateBufferFactory();
  const size_t buffer_size = fbl::round_up(size, zx_system_get_page_size());
  zx_status_t status = buffer_factory->CreateContiguous(bti_, buffer_size, 0, &blk_req_buf_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "cannot alloc blk_req buffers: %s", zx_status_get_string(status));
    return status;
  }
  blk_req_ = static_cast<virtio_blk_req_t*>(blk_req_buf_->virt());

  FDF_LOG(TRACE, "allocated blk request at %p, physical address %#" PRIxPTR "", blk_req_,
          blk_req_buf_->phys());

  // Responses are 32 words at the end of the allocated block.
  blk_res_pa_ = blk_req_buf_->phys() + sizeof(virtio_blk_req_t) * kBlkReqCount;
  blk_res_ = reinterpret_cast<uint8_t*>(
      (reinterpret_cast<uintptr_t>(blk_req_) + sizeof(virtio_blk_req_t) * kBlkReqCount));

  FDF_LOG(TRACE, "allocated blk responses at %p, physical address %#" PRIxPTR "", blk_res_,
          blk_res_pa_);

  StartIrqThread();
  DriverStatusOk();

  auto worker_thread_entry = [](void* ctx) {
    auto bd = static_cast<BlockDevice*>(ctx);
    bd->WorkerThread();
    return ZX_OK;
  };
  int ret =
      thrd_create_with_name(&worker_thread_, worker_thread_entry, this, "virtio-block-worker");
  if (ret != thrd_success) {
    return ZX_ERR_INTERNAL;
  }

  auto watchdog_thread_entry = [](void* ctx) {
    auto bd = static_cast<BlockDevice*>(ctx);
    bd->WatchdogThread();
    return ZX_OK;
  };
  ret = thrd_create_with_name(&watchdog_thread_, watchdog_thread_entry, this,
                              "virtio-block-watchdog");
  if (ret != thrd_success) {
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

void BlockDevice::Release() {
  watchdog_shutdown_.store(true);
  sync_completion_signal(&watchdog_signal_);
  worker_shutdown_.store(true);
  sync_completion_signal(&worker_signal_);
  sync_completion_signal(&txn_signal_);

  thrd_join(watchdog_thread_, nullptr);
  thrd_join(worker_thread_, nullptr);
  virtio::Device::Release();
}

void BlockDevice::IrqRingUpdate() {
  // Parse our descriptor chain and add back to the free queue.
  auto free_chain = [this](vring_used_elem* used_elem) {
    uint32_t i = static_cast<uint16_t>(used_elem->id);
    struct vring_desc* desc = vring_.DescFromIndex(static_cast<uint16_t>(i));
    auto head_desc = desc;  // Save the first element.
    {
      std::lock_guard<std::mutex> lock(ring_lock_);
      for (;;) {
        int next;
        if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
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

    std::optional<uint8_t> status;
    block_txn_t* txn = nullptr;
    {
      std::lock_guard<std::mutex> lock(txn_lock_);

      // Search our pending txn list to see if this completes it.
      list_for_every_entry (&pending_txn_list_, txn, block_txn_t, node) {
        if (txn->desc == head_desc) {
          FDF_LOG(TRACE, "completes txn %p", txn);
          status = blk_res_[txn->req_index];

          free_blk_req(txn->req_index);
          if (txn->discard_req_index) {
            free_blk_req(*txn->discard_req_index);
          }
          list_delete(&txn->node);

          sync_completion_signal(&txn_signal_);
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
  std::optional<size_t> discard_req_index;
  {
    std::lock_guard<std::mutex> lock(txn_lock_);
    req_index = alloc_blk_req();
    if (req_index >= kBlkReqCount) {
      FDF_LOG(TRACE, "too many block requests queued (%zu)!", req_index);
      return ZX_ERR_NO_RESOURCES;
    }
    if (type == VIRTIO_BLK_T_DISCARD) {
      // A second descriptor needs to be allocated for discard requests.
      discard_req_index = alloc_blk_req();
      if (*discard_req_index >= kBlkReqCount) {
        FDF_LOG(TRACE, "too many block requests queued (%zu)!", *discard_req_index);
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
  FDF_LOG(TRACE, "blk_req type %u ioprio %u sector %" PRIu64 "", req->type, req->ioprio,
          req->sector);

  if (type == VIRTIO_BLK_T_DISCARD) {
    // NOTE: if we decide to later send multiple virtio_blk_discard_write_zeroes at once, we must
    // respect the max_discard_seg configuration of the device.
    static_assert(sizeof(virtio_blk_discard_write_zeroes_t) <= sizeof(virtio_blk_req_t));
    virtio_blk_discard_write_zeroes_t* req =
        reinterpret_cast<virtio_blk_discard_write_zeroes_t*>(&blk_req_[req_index]);
    req->sector = txn->op.rw.offset_dev;
    req->num_sectors = txn->op.rw.length;
    FDF_LOG(TRACE, "blk_dwz_req sector %" PRIu64 " num_sectors %" PRIu32, req->sector,
            req->num_sectors);
  }
  // Save the request indexes so we can free them when we complete the transfer.
  txn->req_index = req_index;
  txn->discard_req_index = discard_req_index;

  FDF_LOG(TRACE, "page count %lu", pagecount);

  // Put together a transfer.
  uint16_t i;
  vring_desc* desc;
  uint16_t num_descriptors =
      (type == VIRTIO_BLK_T_DISCARD ? 3u : 2u) + static_cast<uint16_t>(pagecount);
  {
    std::lock_guard<std::mutex> lock(ring_lock_);
    desc = vring_.AllocDescChain(num_descriptors, &i);
  }
  if (!desc) {
    FDF_LOG(TRACE, "failed to allocate descriptor chain of length %zu", 2u + pagecount);
    std::lock_guard<std::mutex> lock(txn_lock_);
    free_blk_req(req_index);
    if (discard_req_index) {
      free_blk_req(*discard_req_index);
    }
    return ZX_ERR_NO_RESOURCES;
  }

  FDF_LOG(TRACE, "after alloc chain desc %p, i %u", desc, i);

  // Point the txn at this head descriptor.
  txn->desc = desc;

  // Set up the descriptor pointing to the head.
  desc->addr = blk_req_buf_->phys() + req_index * sizeof(virtio_blk_req_t);
  desc->len = sizeof(virtio_blk_req_t);
  desc->flags = VRING_DESC_F_NEXT;
  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
    virtio_dump_desc(desc);
  }

  for (size_t n = 0; n < pagecount; n++) {
    desc = vring_.DescFromIndex(desc->next);
    desc->addr = pages[n];  // |pages| are all page-aligned addresses.
    desc->len = static_cast<uint32_t>((bytes > kPageSize) ? kPageSize : bytes);
    if (n == 0) {
      // First entry may not be page aligned.
      size_t page0_offset = txn->op.rw.offset_vmo & kPageMask;

      // Adjust starting address.
      desc->addr += page0_offset;

      // Trim length if necessary.
      size_t max = kPageSize - page0_offset;
      if (desc->len > max) {
        desc->len = static_cast<uint32_t>(max);
      }
    }
    desc->flags = VRING_DESC_F_NEXT;
    FDF_LOG(TRACE, "pa %#lx, len %#x", desc->addr, desc->len);

    // Mark buffer as write-only if its a block read.
    if (type == VIRTIO_BLK_T_IN) {
      desc->flags |= VRING_DESC_F_WRITE;
    }

    bytes -= desc->len;
  }
  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
    virtio_dump_desc(desc);
  }
  assert(bytes == 0);

  if (type == VIRTIO_BLK_T_DISCARD) {
    desc = vring_.DescFromIndex(desc->next);
    desc->addr = blk_req_buf_->phys() + *discard_req_index * sizeof(virtio_blk_req_t);
    desc->len = sizeof(virtio_blk_discard_write_zeroes_t);
    desc->flags = VRING_DESC_F_NEXT;
    if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
      virtio_dump_desc(desc);
    }
  }

  // Set up the descriptor pointing to the response.
  desc = vring_.DescFromIndex(desc->next);
  desc->addr = blk_res_pa_ + req_index;
  desc->len = 1;
  desc->flags = VRING_DESC_F_WRITE;
  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
    virtio_dump_desc(desc);
  }

  {
    std::lock_guard<std::mutex> lock(watchdog_lock_);
    blk_req_start_timestamps_[req_index] = zx::clock::get_monotonic();
  }

  *idx = i;
  return ZX_OK;
}

namespace {
// Out parameter |pages| are all page-aligned addresses.
static zx_status_t pin_pages(zx_handle_t bti, block_txn_t* txn, size_t bytes, zx_paddr_t* pages,
                             size_t* num_pages) {
  uint64_t suboffset = txn->op.rw.offset_vmo & kPageMask;
  uint64_t aligned_offset = txn->op.rw.offset_vmo & ~kPageMask;
  size_t pin_size = ZX_ROUNDUP(suboffset + bytes, kPageSize);
  *num_pages = pin_size / kPageSize;
  if (*num_pages > MAX_SCATTER) {
    FDF_LOG(ERROR, "transaction too large");
    return ZX_ERR_INVALID_ARGS;
  }

  zx_handle_t vmo = txn->op.rw.vmo;
  zx_status_t status = zx_bti_pin(bti, ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, vmo, aligned_offset,
                                  pin_size, pages, *num_pages, &txn->pmt);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "could not pin pages: %s", zx_status_get_string(status));
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}
}  // namespace

void BlockDevice::SignalWorker(block_txn_t* txn) {
  switch (txn->op.command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE:
    case BLOCK_OPCODE_TRIM:
      if (zx_status_t status = block::CheckIoRange(txn->op.rw, config_.capacity, logger());
          status != ZX_OK) {
        txn_complete(txn, status);
        return;
      }
      if (txn->op.command.flags & BLOCK_IO_FLAG_FORCE_ACCESS) {
        txn_complete(txn, ZX_ERR_NOT_SUPPORTED);
        return;
      }
      FDF_LOG(TRACE, "txn %p, opcode %#x\n", txn, txn->op.command.opcode);
      break;
    case BLOCK_OPCODE_FLUSH:
      FDF_LOG(TRACE, "txn %p, opcode FLUSH\n", txn);
      break;
    default:
      txn_complete(txn, ZX_ERR_NOT_SUPPORTED);
      return;
  }

  fbl::AutoLock lock(&lock_);
  if (worker_shutdown_.load()) {
    txn_complete(txn, ZX_ERR_IO_NOT_PRESENT);
    return;
  }
  list_add_tail(&worker_txn_list_, &txn->node);
  sync_completion_signal(&worker_signal_);
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
      fbl::AutoLock lock(&lock_);
      txn = list_remove_head_type(&worker_txn_list_, block_txn_t, node);
    }
    if (!txn) {
      sync_completion_wait(&worker_signal_, ZX_TIME_INFINITE);
      sync_completion_reset(&worker_signal_);
      continue;
    }

    FDF_LOG(TRACE, "WorkerThread handling txn %p", txn);

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
      txn->op.rw.offset_vmo *= config_.blk_size;
      status = pin_pages(bti_.get(), txn, bytes, pages, &num_pages);
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
        std::lock_guard<std::mutex> lock(txn_lock_);
        list_add_tail(&pending_txn_list_, &txn->node);
        vring_.SubmitChain(idx);
        vring_.Kick();
        FDF_LOG(TRACE, "WorkerThread submitted txn %p", txn);
        break;
      }

      if (cannot_fail) {
        FDF_LOG(ERROR, "failed to queue txn to hw: %s", zx_status_get_string(status));
        {
          std::lock_guard<std::mutex> lock(txn_lock_);
          free_blk_req(txn->req_index);
          if (txn->discard_req_index) {
            free_blk_req(*txn->discard_req_index);
          }
        }
        txn_complete(txn, status);
        break;
      }

      {
        std::lock_guard<std::mutex> lock(txn_lock_);
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
        sync_completion_reset(&txn_signal_);
      }

      sync_completion_wait(&txn_signal_, ZX_TIME_INFINITE);
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
    sync_completion_wait(&watchdog_signal_, kWatchdogInterval.get());
    if (watchdog_shutdown_.load()) {
      return;
    }
    zx::time now = zx::clock::get_monotonic();
    {
      std::lock_guard<std::mutex> lock(watchdog_lock_);
      int idx = 0;
      for (const auto& start_time : blk_req_start_timestamps_) {
        if (now - kWatchdogInterval >= start_time) {
          // Round down to the interval
          zx::duration latency = ((now - start_time) / kWatchdogInterval) * kWatchdogInterval;
          // LINT.IfChange(watchdog_tefmo)
          FDF_LOG(WARNING, "txn %d has not completed after %" PRIu64 "s!", idx, latency.to_secs());
          // LINT.ThenChange(/tools/testing/tefmocheck/string_in_log_check.go:watchdog_tefmo)
        }
        idx += 1;
      }
    }
  }
}

void BlockDevice::FlushPendingTxns() {
  for (;;) {
    {
      std::lock_guard<std::mutex> lock(txn_lock_);
      if (list_is_empty(&pending_txn_list_)) {
        return;
      }
      sync_completion_reset(&txn_signal_);
    }
    sync_completion_wait(&txn_signal_, ZX_TIME_INFINITE);
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
    fbl::AutoLock lock(&lock_);
    list_for_every_entry_safe (&worker_txn_list_, txn, temp_entry, block_txn_t, node) {
      list_delete(&txn->node);
      txn_complete(txn, ZX_ERR_IO_NOT_PRESENT);
    }
  }
  std::lock_guard<std::mutex> lock(txn_lock_);
  list_for_every_entry_safe (&pending_txn_list_, txn, temp_entry, block_txn_t, node) {
    free_blk_req(txn->req_index);
    if (txn->discard_req_index) {
      free_blk_req(*txn->discard_req_index);
    }
    list_delete(&txn->node);
    txn_complete(txn, ZX_ERR_IO_NOT_PRESENT);
  }
}

zx::result<> BlockDriver::Start() {
  {
    compat::DeviceServer::BanjoConfig banjo_config;
    banjo_config.callbacks[ZX_PROTOCOL_BLOCK_IMPL] = block_impl_server_.callback();
    zx::result<> result =
        compat_server_.Initialize(incoming(), outgoing(), node_name(), name(),
                                  compat::ForwardMetadata::None(), std::move(banjo_config));
    if (result.is_error()) {
      return result.take_error();
    }
  }

  zx::result device = CreateBlockDevice();
  if (device.is_error()) {
    return device.take_error();
  }
  block_device_ = std::move(*device);

  zx_status_t status = block_device_->Init();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  parent_node_.Bind(std::move(node()));

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  node_controller_.Bind(std::move(controller_client_end));

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia::PROTOCOL,
                                    static_cast<uint32_t>(ZX_PROTOCOL_BLOCK_IMPL));

  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name())
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  auto result = parent_node_->AddChild(args, std::move(controller_server_end), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }
  return zx::ok();
}

zx::result<std::unique_ptr<BlockDevice>> BlockDriver::CreateBlockDevice() {
  zx::result<fidl::ClientEnd<fuchsia_hardware_pci::Device>> pci_client_result =
      incoming()->Connect<fuchsia_hardware_pci::Service::Device>();
  if (pci_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get pci client: %s", pci_client_result.status_string());
    return pci_client_result.take_error();
  }

  zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> bti_and_backend_result =
      virtio::GetBtiAndBackend(ddk::Pci(std::move(pci_client_result).value()));
  if (!bti_and_backend_result.is_ok()) {
    FDF_LOG(ERROR, "GetBtiAndBackend failed: %s", bti_and_backend_result.status_string());
    return bti_and_backend_result.take_error();
  }
  auto [bti, backend] = std::move(bti_and_backend_result).value();

  return zx::ok(std::make_unique<BlockDevice>(std::move(bti), std::move(backend), logger()));
}

void BlockDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  if (block_device_) {
    block_device_->Release();
  }
  completer(zx::ok());
}

void BlockDriver::BlockImplQuery(block_info_t* info, size_t* bopsz) {
  if (block_device_) {
    block_device_->BlockImplQuery(info, bopsz);
  } else {
    FDF_LOG(ERROR, "BlockImplQuery called for driver that has not been started.");
    memset(info, 0, sizeof(*info));
    *bopsz = 0;
  }
}

void BlockDriver::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                 void* cookie) {
  if (block_device_) {
    block_device_->BlockImplQueue(bop, completion_cb, cookie);
  } else {
    FDF_LOG(ERROR, "BlockImplQueue called for driver that has not been started.");
    completion_cb(cookie, ZX_ERR_BAD_STATE, bop);
  }
}

}  // namespace virtio
