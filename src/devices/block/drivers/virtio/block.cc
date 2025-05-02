// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/virtio/block.h"

#include <fidl/fuchsia.hardware.block.volume/cpp/fidl.h>
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

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <mutex>
#include <utility>

#include <fbl/algorithm.h>

#include "src/devices/block/lib/common/include/common.h"
#include "src/storage/lib/block_server/block_server.h"

#define LOCAL_TRACE 0

namespace virtio {

namespace {

// Cache some page size calculations that are used frequently.
const uint32_t kPageSize = zx_system_get_page_size();
const uint32_t kPageMask = kPageSize - 1;
const uint32_t kMaxMaxXfer = (MAX_SCATTER - 1) * kPageSize;

// See 5.2.2 in the virtio spec
const uint16_t kVirtioBlkRequestQueueIndex = 0;

uint32_t VirtioRequestType(const block_txn_t& txn) {
  switch (txn.op.command.opcode) {
    case BLOCK_OPCODE_READ:
      return VIRTIO_BLK_T_IN;
    case BLOCK_OPCODE_WRITE:
      return VIRTIO_BLK_T_OUT;
    case BLOCK_OPCODE_TRIM:
      return VIRTIO_BLK_T_DISCARD;
    case BLOCK_OPCODE_FLUSH:
      return VIRTIO_BLK_T_FLUSH;
    default:
      // Maintain legacy behaviour of defaulting to a read.  Opcodes are untrusted.
      return VIRTIO_BLK_T_IN;
  }
}

uint32_t VirtioRequestType(const block_server::Request& request) {
  switch (request.operation.tag) {
    case block_server::Operation::Tag::Read:
      return VIRTIO_BLK_T_IN;
    case block_server::Operation::Tag::Write:
      return VIRTIO_BLK_T_OUT;
    case block_server::Operation::Tag::Trim:
      return VIRTIO_BLK_T_DISCARD;
    case block_server::Operation::Tag::Flush:
      return VIRTIO_BLK_T_FLUSH;
    case block_server::Operation::Tag::CloseVmo:
      __UNREACHABLE;
  }
}

}  // namespace

void BlockDevice::CompleteTxn(block_txn_t* transaction, zx_status_t status) {
  FDF_LOGL(TRACE, logger(), "Complete txn %p %s", transaction, zx_status_get_string(status));
  if (transaction->pmt != ZX_HANDLE_INVALID) {
    zx_pmt_unpin(transaction->pmt);
    transaction->pmt = ZX_HANDLE_INVALID;
  }
  {
    std::lock_guard<std::mutex> lock(watchdog_lock_);
    blk_req_start_timestamps_[transaction->req_index] = zx::time::infinite();
  }
  // Save the request ID before we release the transaction's resources, because for block server
  // requests, the req_index is used to reserve a slot in our pool for txn, so once we free that,
  // txn could be reused.
  std::optional<uint64_t> request_id = transaction->request;
  {
    std::lock_guard<std::mutex> lock(txn_lock_);
    // NB: req_index might be invalid (>= kBlkReqCount) if transaction comes from Banjo but never
    // even made it as far as allocating a req_index.  That's tolerated by FreeBlkReqLocked.
    FreeBlkReqLocked(transaction->req_index);
    if (transaction->discard_req_index) {
      FreeBlkReqLocked(*transaction->discard_req_index);
    }
    list_delete(&transaction->node);
  }
  txn_cond_.notify_all();
  if (transaction->completion_cb) {
    transaction->completion_cb(transaction->cookie, status, &transaction->op);
  } else {
    ZX_DEBUG_ASSERT(request_id);
    std::lock_guard lock(block_server_lock_);
    if (block_server_) {
      block_server_->SendReply(*request_id, zx::make_result(status));
    }
  }
}

uint32_t BlockDevice::GetMaxTransferSize() const {
  const uint32_t max_transfer_size = static_cast<uint32_t>(kPageSize * (kRingSize - 2));
  // Limit max transfer to our worst case scatter list size.
  return std::min(max_transfer_size, kMaxMaxXfer);
}

flag_t BlockDevice::GetFlags() const {
  flag_t flags = 0;
  if (supports_discard_) {
    flags |= FLAG_TRIM_SUPPORT;
  }
  return flags;
}

void BlockDevice::BlockImplQuery(block_info_t* info, size_t* bopsz) {
  memset(info, 0, sizeof(*info));
  info->block_size = GetBlockSize();
  info->block_count = GetBlockCount();
  info->max_transfer_size = GetMaxTransferSize();
  info->flags = GetFlags();
  *bopsz = sizeof(block_txn_t);
}

void BlockDevice::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                 void* cookie) {
  block_txn_t* txn = static_cast<block_txn_t*>((void*)bop);
  txn->completion_cb = completion_cb;
  txn->cookie = cookie;
  switch (txn->op.command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE:
    case BLOCK_OPCODE_TRIM:
      if (zx_status_t status = block::CheckIoRange(txn->op.rw, config_.capacity, logger());
          status != ZX_OK) {
        completion_cb(cookie, status, bop);
        return;
      }
      if (txn->op.command.flags & BLOCK_IO_FLAG_FORCE_ACCESS) {
        completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, bop);
        return;
      }
      FDF_LOG(TRACE, "txn %p, opcode %#x\n", txn, txn->op.command.opcode);
      break;
    case BLOCK_OPCODE_FLUSH:
      FDF_LOG(TRACE, "txn %p, opcode FLUSH\n", txn);
      break;
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, bop);
      return;
  }
  SignalWorker(txn);
}

void BlockDevice::StartThread(block_server::Thread thread) {
  if (auto server_dispatcher = fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "Virtio Block Server",
          [&](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });
      server_dispatcher.is_ok()) {
    async::PostTask(server_dispatcher->async_dispatcher(),
                    [thread = std::move(thread)]() mutable { thread.Run(); });

    // The dispatcher is destroyed in the shutdown handler.
    server_dispatcher->release();
  }
}

void BlockDevice::OnNewSession(block_server::Session session) {
  if (auto server_dispatcher = fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "Block Server Session",
          [&](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });
      server_dispatcher.is_ok()) {
    async::PostTask(server_dispatcher->async_dispatcher(),
                    [session = std::move(session)]() mutable { session.Run(); });

    // The dispatcher is destroyed in the shutdown handler.
    server_dispatcher->release();
  }
}

void BlockDevice::OnRequests(std::span<block_server::Request> requests) {
  for (auto& request : requests) {
    FDF_LOGL(TRACE, logger(), "Received request %lu", request.request_id);
    if (zx_status_t status = block_server::CheckIoRange(request, config_.capacity);
        status != ZX_OK) {
      FDF_LOGL(WARNING, logger(), "Invalid request range.");
      std::lock_guard lock(block_server_lock_);
      if (block_server_) {
        block_server_->SendReply(request.request_id, zx::make_result(status));
      }
      continue;
    }
    zx::result status = SubmitBlockServerRequest(request);
    if (status.is_error()) {
      FDF_LOGL(WARNING, logger(), "Failed to submit request: %s", status.status_string());
      std::lock_guard lock(block_server_lock_);
      if (block_server_) {
        block_server_->SendReply(request.request_id, status.take_error());
      }
    }
  }
}

void BlockDevice::ServeRequests(fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
  std::lock_guard lock(block_server_lock_);
  if (block_server_) {
    block_server_->Serve(std::move(server_end));
  }
}

BlockDevice::BlockDevice(zx::bti bti, std::unique_ptr<Backend> backend, fdf::Logger& logger)
    : virtio::Device(std::move(bti), std::move(backend)), logger_(logger) {
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
  auto err = vring_.Init(kVirtioBlkRequestQueueIndex, kRingSize);
  if (err < 0) {
    FDF_LOG(ERROR, "failed to allocate vring");
    return err;
  }

  // Allocate a queue of block requests.
  size_t size = sizeof(virtio_blk_req_t) * kBlkReqCount + sizeof(uint8_t) * kBlkReqCount;

  auto buffer_factory = dma_buffer::CreateBufferFactory();
  const size_t buffer_size = fbl::round_up(size, zx_system_get_page_size());
  zx_status_t status = buffer_factory->CreateContiguous(bti_, buffer_size, 0, true, &blk_req_buf_);
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

  {
    std::lock_guard lock(block_server_lock_);
    block_server_.emplace(
        block_server::PartitionInfo{
            .device_flags = GetFlags(),
            .block_count = GetBlockCount(),
            .block_size = GetBlockSize(),
            .max_transfer_size = GetMaxTransferSize(),
        },
        this);
  }

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
  thrd_join(watchdog_thread_, nullptr);

  {
    std::lock_guard lock(block_server_lock_);
    block_server_.reset();
  }
  worker_shutdown_.store(true);
  sync_completion_signal(&worker_signal_);
  txn_cond_.notify_all();
  thrd_join(worker_thread_, nullptr);
  virtio::Device::Release();
}

struct vring_desc* BlockDevice::FreeDescChainLocked(uint16_t index) {
  struct vring_desc* desc = vring_.DescFromIndex(index);
  auto head_desc = desc;  // Save the first element.
  {
    for (;;) {
      std::optional<uint16_t> next;
      if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
        virtio_dump_desc(desc);
      }
      if (desc->flags & VRING_DESC_F_NEXT) {
        next = desc->next;
      }

      vring_.FreeDesc(index);

      if (!next) {
        // End of chain
        break;
      }
      index = *next;
      desc = vring_.DescFromIndex(index);
    }
  }
  return head_desc;
}

void BlockDevice::IrqRingUpdate() {
  // Parse our descriptor chain and add back to the free queue.
  auto free_chain = [this](vring_used_elem* used_elem) {
    std::optional<uint8_t> status;
    block_txn_t* txn = nullptr;
    {
      std::lock_guard<std::mutex> lock(txn_lock_);
      struct vring_desc* head_desc;
      {
        std::lock_guard<std::mutex> lock2(ring_lock_);
        head_desc = FreeDescChainLocked(static_cast<uint16_t>(used_elem->id));
      }

      // Search our pending txn list to see if this completes it.
      list_for_every_entry (&pending_txn_list_, txn, block_txn_t, node) {
        if (txn->desc == head_desc) {
          FDF_LOG(TRACE, "completes txn %p", txn);
          status = blk_res_[txn->req_index];
          // NB: We can't free the transaction's resources until we complete it, because the
          // req_index is used to allocate requests out of the pool.
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
      CompleteTxn(txn, zx_status);
    }
  };

  // Tell the ring to find free chains and hand it back to our lambda.
  vring_.IrqRingUpdate(free_chain);
}

void BlockDevice::IrqConfigChange() {}

void BlockDevice::QueueTxn(block_txn_t* txn, RequestContext context) {
  ZX_DEBUG_ASSERT(txn);

  uint32_t type = VirtioRequestType(*txn);
  txn->req_index = context.req_index();
  txn->discard_req_index = context.discard_req_index();
  txn->desc = context.desc();
  txn->pmt = context.pmt();

  auto req = &blk_req_[txn->req_index];
  req->type = type;
  req->ioprio = 0;
  if (req->type == VIRTIO_BLK_T_FLUSH) {
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
        reinterpret_cast<virtio_blk_discard_write_zeroes_t*>(&blk_req_[*txn->discard_req_index]);
    req->sector = txn->op.trim.offset_dev;
    req->num_sectors = txn->op.trim.length;
    req->flags = 0;
    FDF_LOG(TRACE, "blk_dwz_req sector %" PRIu64 " num_sectors %" PRIu32, req->sector,
            req->num_sectors);
  }
  FDF_LOG(TRACE, "page count %lu", context.num_pages());

  // Set up the head descriptor.
  struct vring_desc* desc = context.desc();
  desc->addr = blk_req_buf_->phys() + txn->req_index * sizeof(virtio_blk_req_t);
  desc->len = sizeof(virtio_blk_req_t);
  desc->flags = VRING_DESC_F_NEXT;
  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
    virtio_dump_desc(txn->desc);
  }

  size_t bytes = type == VIRTIO_BLK_T_IN || type == VIRTIO_BLK_T_OUT
                     ? txn->op.rw.length * config_.blk_size
                     : 0;
  for (size_t n = 0; n < context.num_pages(); n++) {
    desc = vring_.DescFromIndex(desc->next);
    desc->addr = context.pages()[n];  // |pages| are all page-aligned addresses.
    desc->len = static_cast<uint32_t>((bytes > kPageSize) ? kPageSize : bytes);
    if (n == 0) {
      // First entry may not be page aligned.
      size_t page0_offset = (txn->op.rw.offset_vmo * config_.blk_size) & kPageMask;

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
  assert(bytes == 0);

  if (type == VIRTIO_BLK_T_DISCARD) {
    desc = vring_.DescFromIndex(desc->next);
    desc->addr = blk_req_buf_->phys() + *context.discard_req_index() * sizeof(virtio_blk_req_t);
    desc->len = sizeof(virtio_blk_discard_write_zeroes_t);
    desc->flags = VRING_DESC_F_NEXT;
    if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
      virtio_dump_desc(desc);
    }
  }

  // Set up the descriptor pointing to the response.
  desc = vring_.DescFromIndex(desc->next);
  desc->addr = blk_res_pa_ + context.req_index();
  desc->len = 1;
  desc->flags = VRING_DESC_F_WRITE;
  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
    virtio_dump_desc(desc);
  }

  {
    std::lock_guard<std::mutex> lock(watchdog_lock_);
    blk_req_start_timestamps_[context.req_index()] = zx::clock::get_monotonic();
  }

  std::lock_guard<std::mutex> lock(txn_lock_);
  list_add_tail(&pending_txn_list_, &txn->node);
  vring_.SubmitChain(context.desc_index());
  vring_.Kick();
  FDF_LOG(TRACE, "Submitted txn %p (desc %u)", txn, context.desc_index());
  context.Release();
}

zx::result<zx_handle_t> BlockDevice::PinPages(zx_handle_t bti, zx_handle_t vmo,
                                              uint64_t vmo_offset_blocks, uint32_t num_blocks,
                                              std::array<zx_paddr_t, MAX_SCATTER>* pages,
                                              size_t* num_pages) const {
  uint64_t suboffset = (vmo_offset_blocks * config_.blk_size) & kPageMask;
  uint64_t aligned_offset = (vmo_offset_blocks * config_.blk_size) & ~kPageMask;
  size_t pin_size =
      ZX_ROUNDUP(suboffset + (static_cast<uint64_t>(num_blocks * config_.blk_size)), kPageSize);
  *num_pages = pin_size / kPageSize;
  if (*num_pages > pages->size()) {
    FDF_LOG(ERROR, "transaction too large");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  zx_handle_t pmt;
  zx_status_t status = zx_bti_pin(bti, ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, vmo, aligned_offset,
                                  pin_size, pages->data(), *num_pages, &pmt);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "could not pin pages: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(pmt);
}

zx::result<> BlockDevice::SubmitBlockServerRequest(const block_server::Request& request) {
  uint32_t type = VirtioRequestType(request);
  std::array<zx_paddr_t, MAX_SCATTER> pages;
  zx::result<RequestContext> context = AllocateRequestContext(
      type, request.vmo->get(), request.operation.read.vmo_offset / config_.blk_size,
      request.operation.read.block_count, &pages);
  if (context.is_error()) {
    return context.take_error();
  }
  block_txn_t* txn = &block_server_request_pool_[context->req_index()];

  txn->request = request.request_id;
  txn->completion_cb = nullptr;  // Not used for block_server requests

  switch (request.operation.tag) {
    case block_server::Operation::Tag::Read:
      txn->op.rw.command.opcode = BLOCK_OPCODE_READ;
      txn->op.rw.vmo = request.vmo->get();
      txn->op.rw.length = request.operation.read.block_count;
      txn->op.rw.offset_dev = request.operation.read.device_block_offset;
      txn->op.rw.offset_vmo = request.operation.read.vmo_offset / config_.blk_size;
      break;
    case block_server::Operation::Tag::Write:
      txn->op.rw.command.opcode = BLOCK_OPCODE_WRITE;
      if (request.operation.write.options.is_force_access()) {
        txn->op.rw.command.flags |= BLOCK_IO_FLAG_FORCE_ACCESS;
      }
      txn->op.rw.vmo = request.vmo->get();
      txn->op.rw.length = request.operation.write.block_count;
      txn->op.rw.offset_dev = request.operation.write.device_block_offset;
      txn->op.rw.offset_vmo = request.operation.write.vmo_offset / config_.blk_size;
      break;
    case block_server::Operation::Tag::Trim:
      txn->op.trim.command.opcode = BLOCK_OPCODE_TRIM;
      txn->op.trim.length = request.operation.trim.block_count;
      txn->op.trim.offset_dev = request.operation.trim.device_block_offset;
      break;
    case block_server::Operation::Tag::Flush:
      txn->op.command.opcode = BLOCK_OPCODE_FLUSH;
      break;
    case block_server::Operation::Tag::CloseVmo:
      // The rust block server will never send CloseVmo to its C interface.
      __UNREACHABLE;
  }

  // A flush operation should complete after any in-flight transactions, so wait for all pending
  // txns to complete before submitting a flush txn. This is necessary because a virtio block device
  // may service requests in any order.
  if (type == VIRTIO_BLK_T_FLUSH) {
    FlushPendingTxns();
    if (worker_shutdown_.load()) {
      std::lock_guard lock1(txn_lock_);
      std::lock_guard lock2(ring_lock_);
      FreeRequestContext(*context);
      return zx::error(ZX_ERR_IO_NOT_PRESENT);
    }
  }

  QueueTxn(txn, std::move(*context));

  // A flush operation should complete before any subsequent transactions. So, we wait for all
  // pending transactions (including the flush) to complete before continuing.
  if (type == VIRTIO_BLK_T_FLUSH) {
    FlushPendingTxns();
  }

  return zx::ok();
}

BlockDevice::RequestContext::RequestContext(BlockDevice::RequestContext&& other) {
  *this = std::move(other);
}

BlockDevice::RequestContext& BlockDevice::RequestContext::operator=(RequestContext&& other) {
  if (this != &other) {
    released_ = other.released_;
    req_index_ = other.req_index_;
    discard_req_index_ = other.discard_req_index_;
    desc_index_ = other.desc_index_;
    desc_ = other.desc_;
    pages_ = other.pages_;
    num_pages_ = other.num_pages_;
    pmt_ = other.pmt_;
    other.Release();
  }
  return *this;
}

BlockDevice::RequestContext::~RequestContext() {
  // RAII-style cleanup isn't a good option, because the destructor would need to take locks (see
  // BlockDevice::FreeRequestContext), which could cause difficult-to-spot deadlocks.
  ZX_ASSERT_MSG(released_, "Did you forget to call Release/FreeRequestContext?");
}

void BlockDevice::FreeRequestContext(BlockDevice::RequestContext& context) {
  if (context.pmt() != ZX_HANDLE_INVALID) {
    zx_pmt_unpin(context.pmt());
  }
  if (context.desc() != nullptr) {
    FreeDescChainLocked(context.desc_index());
  }
  if (context.req_index() < kBlkReqCount) {
    FreeBlkReqLocked(context.req_index());
    if (context.discard_req_index()) {
      FreeBlkReqLocked(*context.discard_req_index());
    }
  }
  context.Release();
  txn_cond_.notify_all();
}

void BlockDevice::RequestContext::Release() {
  ZX_ASSERT_MSG(!released_, "Release called twice");
  released_ = true;
}

void BlockDevice::SignalWorker(block_txn_t* txn) {
  std::lock_guard lock(lock_);
  if (worker_shutdown_.load()) {
    CompleteTxn(txn, ZX_ERR_IO_NOT_PRESENT);
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
      std::lock_guard lock(lock_);
      txn = list_remove_head_type(&worker_txn_list_, block_txn_t, node);
    }
    if (!txn) {
      sync_completion_wait(&worker_signal_, ZX_TIME_INFINITE);
      sync_completion_reset(&worker_signal_);
      continue;
    }

    FDF_LOG(TRACE, "WorkerThread handling txn %p", txn);
    if (zx::result result = SubmitBanjoRequest(txn); result.is_error()) {
      if (txn->completion_cb) {
        txn->completion_cb(txn->cookie, result.status_value(), &txn->op);
      }
    }
  }
}

zx::result<> BlockDevice::SubmitBanjoRequest(block_txn_t* transaction) {
  uint32_t type = VirtioRequestType(*transaction);
  std::array<zx_paddr_t, MAX_SCATTER> pages;
  zx::result<RequestContext> context =
      AllocateRequestContext(type, transaction->op.rw.vmo, transaction->op.rw.offset_vmo,
                             transaction->op.rw.length, &pages);
  if (context.is_error()) {
    FDF_LOG(ERROR, "failed to queue txn to hw: %s", context.status_string());
    return context.take_error();
  }

  // A flush operation should complete after any in-flight transactions, so wait for all pending
  // txns to complete before submitting a flush txn. This is necessary because a virtio block
  // device may service requests in any order.
  if (type == VIRTIO_BLK_T_FLUSH) {
    FlushPendingTxns();
    if (worker_shutdown_.load()) {
      std::lock_guard lock1(txn_lock_);
      std::lock_guard lock2(ring_lock_);
      FreeRequestContext(*context);
      return zx::error(ZX_ERR_IO_NOT_PRESENT);
    }
  }

  QueueTxn(transaction, std::move(*context));

  // A flush operation should complete before any subsequent transactions. So, we wait for all
  // pending transactions (including the flush) to complete before continuing.
  if (type == VIRTIO_BLK_T_FLUSH) {
    FlushPendingTxns();
  }

  return zx::ok();
}

// Thread safety: Disable thread safety analysis because TA doesn't understand std::unique_lock,
// which is required by std::condition_variable.
zx::result<BlockDevice::RequestContext> BlockDevice::AllocateRequestContext(
    uint32_t type, zx_handle_t vmo, uint64_t vmo_offset_blocks, uint32_t num_blocks,
    std::array<zx_paddr_t, MAX_SCATTER>* pages) TA_NO_THREAD_SAFETY_ANALYSIS {
  for (;;) {
    std::unique_lock lock(txn_lock_);
    if (worker_shutdown_.load()) {
      return zx::error(ZX_ERR_IO_NOT_PRESENT);
    }
    zx::result<std::optional<RequestContext>> result;
    {
      std::unique_lock lock2(ring_lock_);
      result = TryAllocateRequestContextLocked(type, vmo, vmo_offset_blocks, num_blocks, pages);
    }
    if (result.is_error()) {
      FDF_LOG(ERROR, "failed to allocate virtio resources: %s", result.status_string());
      return result.take_error();
    }
    if (result.value().has_value()) {
      return zx::ok(std::move(*result).value());
    }

    // No resources; try again.
    txn_cond_.wait(lock);
  }
}

zx::result<std::optional<BlockDevice::RequestContext>> BlockDevice::TryAllocateRequestContextLocked(
    uint32_t type, zx_handle_t vmo, uint64_t vmo_offset_blocks, uint32_t num_blocks,
    std::array<zx_paddr_t, MAX_SCATTER>* pages) {
  RequestContext out;
  // Thread safety: TryAllocateRequestContextLocked requires the necessary locks.
  auto cleanup = fit::defer([&]() TA_NO_THREAD_SAFETY_ANALYSIS { FreeRequestContext(out); });
  std::optional<size_t> idx = AllocateBlkReqLocked();
  if (!idx) {
    FDF_LOG(TRACE, "too many block requests queued!");
    return zx::ok(std::nullopt);
  }
  out.set_req_index(*idx);

  if (type == VIRTIO_BLK_T_DISCARD) {
    // A second descriptor needs to be allocated for discard requests.
    idx = AllocateBlkReqLocked();
    if (!idx) {
      FDF_LOG(TRACE, "too many block requests queued!");
      return zx::ok(std::nullopt);
    }
    out.set_discard_req_index(*idx);
  }

  if (type == VIRTIO_BLK_T_IN || type == VIRTIO_BLK_T_OUT) {
    size_t num_pages;
    zx::result result = PinPages(bti_.get(), vmo, vmo_offset_blocks, num_blocks, pages, &num_pages);
    if (result.is_error()) {
      return result.take_error();
    }
    out.set_pages(pages->data(), num_pages);
    out.set_pmt(result.value());
  }

  uint16_t num_descriptors =
      (type == VIRTIO_BLK_T_DISCARD ? 3u : 2u) + static_cast<uint16_t>(out.num_pages());
  uint16_t desc_index;
  struct vring_desc* desc = nullptr;
  desc = vring_.AllocDescChain(num_descriptors, &desc_index);
  if (!desc) {
    FDF_LOG(TRACE, "failed to allocate descriptor chain of length %zu", 2u + out.num_pages());
    return zx::ok(std::nullopt);
  }
  out.set_desc(desc, desc_index);
  cleanup.cancel();
  return zx::ok(std::move(out));
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
  std::unique_lock lock(txn_lock_);
  txn_cond_.wait(lock,
                 [&]() { return worker_shutdown_.load() || list_is_empty(&pending_txn_list_); });
}

void BlockDevice::CleanupPendingTxns() {
  // Virtio specification 3.3.1 Driver Requirements: Device Cleanup
  // A driver MUST ensure a virtqueue isn’t live (by device reset) before removing exposed
  // buffers.
  DeviceReset();
  block_txn_t* txn = nullptr;
  block_txn_t* temp_entry = nullptr;
  {
    std::lock_guard lock(lock_);
    list_for_every_entry_safe (&worker_txn_list_, txn, temp_entry, block_txn_t, node) {
      list_delete(&txn->node);
      CompleteTxn(txn, ZX_ERR_IO_NOT_PRESENT);
    }
  }
  std::lock_guard<std::mutex> lock(txn_lock_);
  list_for_every_entry_safe (&pending_txn_list_, txn, temp_entry, block_txn_t, node) {
    FreeBlkReqLocked(txn->req_index);
    if (txn->discard_req_index) {
      FreeBlkReqLocked(*txn->discard_req_index);
    }
    list_delete(&txn->node);
    CompleteTxn(txn, ZX_ERR_IO_NOT_PRESENT);
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

  if (zx::result result = outgoing()->AddService<fuchsia_hardware_block_volume::Service>(
          fuchsia_hardware_block_volume::Service::InstanceHandler({
              .volume =
                  [this](fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
                    block_device_->ServeRequests(std::move(server_end));
                  },
          }));
      result.is_error()) {
    FDF_LOGL(ERROR, logger(), "Failed to add volume service instance: %s", result.status_string());
    return result.take_error();
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
      virtio::GetBtiAndBackend(std::move(pci_client_result).value());
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
