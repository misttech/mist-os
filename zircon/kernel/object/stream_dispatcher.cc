// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "object/stream_dispatcher.h"

#include <lib/counters.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <ktl/algorithm.h>
#include <ktl/atomic.h>
#include <object/vm_object_dispatcher.h>

#include <ktl/enforce.h>

KCOUNTER(dispatcher_stream_create_count, "dispatcher.stream.create")
KCOUNTER(dispatcher_stream_destroy_count, "dispatcher.stream.destroy")

// static
zx_status_t StreamDispatcher::parse_create_syscall_flags(uint32_t flags, uint32_t* out_flags,
                                                         zx_rights_t* out_required_vmo_rights) {
  uint32_t res = 0;
  zx_rights_t required_vmo_rights = ZX_RIGHT_NONE;
  if (flags & ZX_STREAM_MODE_READ) {
    res |= kModeRead;
    required_vmo_rights |= ZX_RIGHT_READ;
    flags &= ~ZX_STREAM_MODE_READ;
  }
  if (flags & ZX_STREAM_MODE_WRITE) {
    res |= kModeWrite;
    required_vmo_rights |= ZX_RIGHT_WRITE;
    flags &= ~ZX_STREAM_MODE_WRITE;
  }
  if (flags & ZX_STREAM_MODE_APPEND) {
    res |= kModeAppend;
    flags &= ~ZX_STREAM_MODE_APPEND;
  }

  if (flags) {
    return ZX_ERR_INVALID_ARGS;
  }

  *out_flags = res;
  *out_required_vmo_rights = required_vmo_rights;
  return ZX_OK;
}

// static
zx_status_t StreamDispatcher::Create(uint32_t options, fbl::RefPtr<VmObjectPaged> vmo,
                                     fbl::RefPtr<ContentSizeManager> csm, zx_off_t seek,
                                     KernelHandle<StreamDispatcher>* handle, zx_rights_t* rights) {
  fbl::AllocChecker ac;
  KernelHandle new_handle(
      fbl::AdoptRef(new (&ac) StreamDispatcher(options, ktl::move(vmo), ktl::move(csm), seek)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_rights_t new_rights = default_rights();

  if (options & kModeRead) {
    new_rights |= ZX_RIGHT_READ;
  }
  if (options & kModeWrite) {
    new_rights |= ZX_RIGHT_WRITE;
  }

  *rights = new_rights;
  *handle = ktl::move(new_handle);
  return ZX_OK;
}

StreamDispatcher::StreamDispatcher(uint32_t options, fbl::RefPtr<VmObjectPaged> vmo,
                                   fbl::RefPtr<ContentSizeManager> content_size_mgr, zx_off_t seek)
    : options_(options),
      vmo_(ktl::move(vmo)),
      content_size_mgr_(ktl::move(content_size_mgr)),
      seek_(seek) {
  kcounter_add(dispatcher_stream_create_count, 1);
  (void)options_;
}

StreamDispatcher::~StreamDispatcher() { kcounter_add(dispatcher_stream_destroy_count, 1); }

zx_status_t StreamDispatcher::ReadVector(user_out_iovec_t user_data, size_t* out_actual) {
  canary_.Assert();
  DEBUG_ASSERT(out_actual);
  DEBUG_ASSERT(*out_actual == 0);

  size_t total_capacity;
  zx_status_t status = user_data.GetTotalCapacity(&total_capacity);
  if (status != ZX_OK) {
    return status;
  }
  if (total_capacity == 0) {
    return ZX_OK;
  }

  size_t length = 0u;
  uint64_t offset = 0u;
  ContentSizeManager::Operation op(content_size_mgr_.get());

  Guard<Mutex> seek_guard{&seek_lock_};
  {
    Guard<Mutex> content_size_guard{AliasedLock, content_size_mgr_->lock(), op.lock()};

    uint64_t size_limit = 0u;
    content_size_mgr_->BeginReadLocked(seek_ + total_capacity, &size_limit, &op);
    if (size_limit <= seek_) {
      // Return |ZX_OK| since there is nothing to be read.
      op.CancelLocked();
      return ZX_OK;
    }

    offset = seek_;
    length = size_limit - offset;
  }

  status = vmo_->ReadUserVector(user_data, offset, length, out_actual);
  seek_ += *out_actual;

  // Reacquire the lock to commit the operation.
  Guard<Mutex> content_size_guard{op.lock()};
  op.CommitLocked();

  return *out_actual > 0 ? ZX_OK : status;
}

zx_status_t StreamDispatcher::ReadVectorAt(user_out_iovec_t user_data, zx_off_t offset,
                                           size_t* out_actual) {
  canary_.Assert();
  DEBUG_ASSERT(out_actual);
  DEBUG_ASSERT(*out_actual == 0);

  size_t total_capacity;
  zx_status_t status = user_data.GetTotalCapacity(&total_capacity);
  if (status != ZX_OK) {
    return status;
  }
  if (total_capacity == 0) {
    return ZX_OK;
  }

  size_t length = 0u;
  ContentSizeManager::Operation op(content_size_mgr_.get());

  {
    Guard<Mutex> content_size_guard{AliasedLock, content_size_mgr_->lock(), op.lock()};

    uint64_t size_limit = 0u;
    content_size_mgr_->BeginReadLocked(offset + total_capacity, &size_limit, &op);
    if (size_limit <= offset) {
      // Return |ZX_OK| since there is nothing to be read.
      op.CancelLocked();
      return ZX_OK;
    }

    length = size_limit - offset;
  }

  status = vmo_->ReadUserVector(user_data, offset, length, out_actual);

  // Reacquire the lock to commit the operation.
  Guard<Mutex> content_size_guard{op.lock()};
  op.CommitLocked();

  return *out_actual > 0 ? ZX_OK : status;
}

zx_status_t StreamDispatcher::WriteVector(user_in_iovec_t user_data, size_t* out_actual) {
  canary_.Assert();
  DEBUG_ASSERT(out_actual);
  DEBUG_ASSERT(*out_actual == 0);

  if (IsInAppendMode()) {
    return AppendVector(user_data, out_actual);
  }

  size_t total_capacity;
  zx_status_t status = user_data.GetTotalCapacity(&total_capacity);
  if (status != ZX_OK) {
    return status;
  }

  // Return early if writing zero bytes since there's nothing to do.
  if (total_capacity == 0) {
    return ZX_OK;
  }

  size_t length = 0u;
  ContentSizeManager::Operation op(content_size_mgr_.get());
  ktl::optional<uint64_t> prev_content_size;

  Guard<Mutex> seek_guard{&seek_lock_};

  status = CreateWriteOpAndExpandVmo(total_capacity, seek_, &length, &prev_content_size, &op);
  if (status != ZX_OK) {
    return status;
  }

  if (prev_content_size) {
    status = vmo_->WriteUserVector(
        user_data, seek_, length, out_actual,
        [&prev_content_size, &op](const uint64_t write_offset, const size_t len) {
          if (write_offset + len > *prev_content_size) {
            op.UpdateContentSizeFromProgress(write_offset + len);
          }
        });
  } else {
    status = vmo_->WriteUserVector(user_data, seek_, length, out_actual, nullptr);
  }

  // Reacquire the lock to potentially shrink and commit the operation.
  Guard<Mutex> content_size_guard{op.lock()};

  // Update the content size operation if operation was partially successful.
  if (*out_actual < length) {
    DEBUG_ASSERT(status != ZX_OK);

    if (*out_actual == 0u) {
      // Do not commit the operation if nothing was written.
      op.CancelLocked();
      return status;
    } else {
      op.ShrinkSizeLocked(seek_ + *out_actual);
    }
  }

  seek_ += *out_actual;

  op.CommitLocked();
  return *out_actual > 0 ? ZX_OK : status;
}

zx_status_t StreamDispatcher::WriteVectorAt(user_in_iovec_t user_data, zx_off_t offset,
                                            size_t* out_actual) {
  canary_.Assert();
  DEBUG_ASSERT(out_actual);
  DEBUG_ASSERT(*out_actual == 0);

  size_t total_capacity;
  zx_status_t status = user_data.GetTotalCapacity(&total_capacity);
  if (status != ZX_OK) {
    return status;
  }

  // Return early if writing zero bytes
  if (total_capacity == 0) {
    return ZX_OK;
  }

  size_t length = 0u;
  ContentSizeManager::Operation op(content_size_mgr_.get());
  ktl::optional<uint64_t> prev_content_size;

  status = CreateWriteOpAndExpandVmo(total_capacity, offset, &length, &prev_content_size, &op);
  if (status != ZX_OK) {
    return status;
  }

  if (prev_content_size) {
    status = vmo_->WriteUserVector(
        user_data, offset, length, out_actual,
        [&prev_content_size, &op](const uint64_t write_offset, const size_t len) {
          if (write_offset + len > *prev_content_size) {
            op.UpdateContentSizeFromProgress(write_offset + len);
          }
        });
  } else {
    status = vmo_->WriteUserVector(user_data, offset, length, out_actual, nullptr);
  }

  // Reacquire the lock to potentially shrink and commit the operation.
  Guard<Mutex> content_size_guard{op.lock()};

  // Update the content size operation if operation was partially successful.
  if (*out_actual < length) {
    DEBUG_ASSERT(status != ZX_OK);

    if (*out_actual == 0u) {
      // Do not commit the operation if nothing was written.
      op.CancelLocked();
      return status;
    } else {
      op.ShrinkSizeLocked(offset + *out_actual);
    }
  }

  op.CommitLocked();
  return *out_actual > 0 ? ZX_OK : status;
}

zx_status_t StreamDispatcher::AppendVector(user_in_iovec_t user_data, size_t* out_actual) {
  canary_.Assert();
  DEBUG_ASSERT(out_actual);
  DEBUG_ASSERT(*out_actual == 0);

  size_t total_capacity;
  zx_status_t status = user_data.GetTotalCapacity(&total_capacity);
  if (status != ZX_OK) {
    return status;
  }

  // Return early if writing zero bytes since there's nothing to do.
  if (total_capacity == 0) {
    return ZX_OK;
  }

  const bool can_resize_vmo = CanResizeVmo();

  size_t length = 0u;
  uint64_t offset = 0u;
  ContentSizeManager::Operation op(content_size_mgr_.get());
  Guard<Mutex> seek_guard{&seek_lock_};

  // This section expands the VMO if necessary and bumps the |seek_| pointer if successful.
  {
    Guard<Mutex> content_size_guard{AliasedLock, content_size_mgr_->lock(), op.lock()};

    status = content_size_mgr_->BeginAppendLocked(total_capacity, &content_size_guard, &op);
    if (status != ZX_OK) {
      return status;
    }

    uint64_t new_content_size = op.GetSizeLocked();

    offset = new_content_size - total_capacity;

    uint64_t vmo_size = 0u;
    status = ExpandIfNecessary(new_content_size, can_resize_vmo, &vmo_size);
    if (status != ZX_OK) {
      if (vmo_size <= offset) {
        // Unable to expand to requested size and cannot even perform partial write.
        op.CancelLocked();

        // Return `ZX_ERR_OUT_OF_RANGE` for range errors. Otherwise, clients expect all other errors
        // related to resize failure to be `ZX_ERR_NO_SPACE`.
        return status == ZX_ERR_OUT_OF_RANGE ? status : ZX_ERR_NO_SPACE;
      }
    }

    DEBUG_ASSERT(vmo_size > offset);

    if (vmo_size < new_content_size) {
      // Unable to expand to requested size but able to perform a partial write.
      op.ShrinkSizeLocked(vmo_size);
    }

    length = ktl::min(vmo_size, new_content_size) - offset;
  }

  status = vmo_->WriteUserVector(user_data, offset, length, out_actual,
                                 [&op](const uint64_t write_offset, const size_t len) {
                                   op.UpdateContentSizeFromProgress(write_offset + len);
                                 });
  seek_ = offset + *out_actual;

  // Reacquire the lock to potentially shrink and commit the operation.
  Guard<Mutex> content_size_guard{AliasedLock, content_size_mgr_->lock(), op.lock()};

  // Update the content size operation if operation was partially successful.
  if (*out_actual < length) {
    DEBUG_ASSERT(status != ZX_OK);

    if (*out_actual == 0) {
      // Do not commit the operation if nothing was written.
      op.CancelLocked();
      return status;
    } else {
      op.ShrinkSizeLocked(offset + *out_actual);
    }
  }

  op.CommitLocked();
  return *out_actual > 0 ? ZX_OK : status;
}

zx_status_t StreamDispatcher::Seek(zx_stream_seek_origin_t whence, int64_t offset,
                                   zx_off_t* out_seek) {
  canary_.Assert();

  Guard<Mutex> seek_guard{&seek_lock_};

  zx_off_t target;
  switch (whence) {
    case ZX_STREAM_SEEK_ORIGIN_START: {
      if (offset < 0) {
        return ZX_ERR_INVALID_ARGS;
      }
      target = static_cast<zx_off_t>(offset);
      break;
    }
    case ZX_STREAM_SEEK_ORIGIN_CURRENT: {
      if (add_overflow(seek_, offset, &target)) {
        return ZX_ERR_INVALID_ARGS;
      }
      break;
    }
    case ZX_STREAM_SEEK_ORIGIN_END: {
      uint64_t content_size = content_size_mgr_->GetContentSize();
      if (add_overflow(content_size, offset, &target)) {
        return ZX_ERR_INVALID_ARGS;
      }
      break;
    }
    default: {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  seek_ = target;
  *out_seek = seek_;
  return ZX_OK;
}

zx_status_t StreamDispatcher::SetAppendMode(bool value) {
  Guard<CriticalMutex> guard{get_lock()};
  options_ = (options_ & ~kModeAppend) | (value ? kModeAppend : 0);
  return ZX_OK;
}

bool StreamDispatcher::IsInAppendMode() const {
  Guard<CriticalMutex> guard{get_lock()};
  return options_ & kModeAppend;
}

void StreamDispatcher::GetInfo(zx_info_stream_t* info) const {
  canary_.Assert();

  Guard<CriticalMutex> options_guard{get_lock()};
  Guard<Mutex> seek_guard{&seek_lock_};

  info->options = 0;
  if (options_ & kModeRead) {
    info->options |= ZX_STREAM_MODE_READ;
  }
  if (options_ & kModeWrite) {
    info->options |= ZX_STREAM_MODE_WRITE;
  }
  if (options_ & kModeAppend) {
    info->options |= ZX_STREAM_MODE_APPEND;
  }

  info->seek = seek_;
  info->content_size = content_size_mgr_->GetContentSize();
}

bool StreamDispatcher::CanResizeVmo() const {
  Guard<CriticalMutex> guard{get_lock()};
  return options_ & kCanResizeVmo;
}

zx_status_t StreamDispatcher::CreateWriteOpAndExpandVmo(
    size_t total_capacity, zx_off_t offset, uint64_t* out_length,
    ktl::optional<uint64_t>* out_prev_content_size, ContentSizeManager::Operation* out_op) {
  DEBUG_ASSERT(out_op);
  DEBUG_ASSERT(out_length);

  zx_status_t status = ZX_OK;
  const bool can_resize_vmo = CanResizeVmo();

  {
    Guard<Mutex> content_size_guard{AliasedLock, content_size_mgr_->lock(), out_op->lock()};

    size_t requested_content_size;
    if (add_overflow(offset, total_capacity, &requested_content_size)) {
      return ZX_ERR_FILE_BIG;
    }

    content_size_mgr_->BeginWriteLocked(requested_content_size, &content_size_guard,
                                        out_prev_content_size, out_op);

    uint64_t vmo_size = 0u;
    status = ExpandIfNecessary(requested_content_size, can_resize_vmo, &vmo_size);
    if (status != ZX_OK) {
      if (vmo_size <= offset) {
        // Unable to expand to requested size and cannot even perform partial write.
        out_op->CancelLocked();

        // Return `ZX_ERR_OUT_OF_RANGE` for range errors. Otherwise, clients expect all other errors
        // related to resize failure to be `ZX_ERR_NO_SPACE`.
        return status == ZX_ERR_OUT_OF_RANGE ? status : ZX_ERR_NO_SPACE;
      }
    }

    DEBUG_ASSERT(vmo_size > offset);

    // Allow writing up to the minimum of the VMO size and requested content size, since we want to
    // write at most the requested size but don't want to write beyond the VMO size.
    const uint64_t target_content_size = ktl::min(vmo_size, requested_content_size);
    *out_length = target_content_size - offset;

    if (target_content_size != requested_content_size) {
      out_op->ShrinkSizeLocked(target_content_size);
    }
  }

  // Zero content between the previous content size and the start of the write.
  if (out_prev_content_size->has_value() && out_prev_content_size->value() < offset) {
    status =
        vmo_->ZeroRange(out_prev_content_size->value(), offset - out_prev_content_size->value());
    if (status != ZX_OK) {
      Guard<Mutex> content_size_guard{out_op->lock()};
      out_op->CancelLocked();
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t StreamDispatcher::ExpandIfNecessary(uint64_t requested_vmo_size, bool can_resize_vmo,
                                                uint64_t* out_actual) {
  uint64_t current_vmo_size = vmo_->size();
  *out_actual = current_vmo_size;

  uint64_t required_vmo_size = ROUNDUP(requested_vmo_size, PAGE_SIZE);
  // Overflow when rounding up.
  if (required_vmo_size < requested_vmo_size) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (required_vmo_size > current_vmo_size) {
    if (!can_resize_vmo) {
      return ZX_ERR_NOT_SUPPORTED;
    }
    zx_status_t status = vmo_->Resize(required_vmo_size);
    if (status != ZX_OK) {
      // Resizing failed but the rest of the current VMO size can be used.
      return status;
    }

    *out_actual = required_vmo_size;
  }

  return ZX_OK;
}
