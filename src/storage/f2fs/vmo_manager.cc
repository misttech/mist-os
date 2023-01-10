// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

VmoMapping::VmoMapping(zx::vmo &unowned_vmo, pgoff_t index, size_t size, bool zero)
    : size_in_blocks_(size), index_(index), vmo_(unowned_vmo) {
  zx_vaddr_t addr;
  size_t start_offset = 0;
  if (!unowned_vmo.is_valid()) {
    ZX_ASSERT(zx::vmo::create(page_to_address(get_size()), ZX_VMO_DISCARDABLE, &owned_vmo_) ==
              ZX_OK);
    ZX_ASSERT(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_ALLOW_FAULTS, 0,
                                         vmo(), 0, page_to_address(get_size()), &addr) == ZX_OK);
  } else {
    start_offset = page_to_address(index);
    ZX_ASSERT(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_ALLOW_FAULTS, 0,
                                         vmo(), page_to_address(index), page_to_address(get_size()),
                                         &addr) == ZX_OK);
  }
  if (zero) {
    ZX_ASSERT(vmo().op_range(ZX_VMO_OP_ZERO, start_offset, page_to_address(get_size()), nullptr,
                             0) == ZX_OK);
  }
  address_ = addr;
}

VmoMapping::~VmoMapping() {
  if (address()) {
    ZX_ASSERT(zx::vmar::root_self()->unmap(address(), page_to_address(get_size())) == ZX_OK);
  }
}

zx_vaddr_t VmoMapping::page_to_address(pgoff_t page_index) const {
  return safemath::CheckMul<zx_vaddr_t>(page_index, kPageSize).ValueOrDie();
}

pgoff_t VmoMapping::address_to_page(zx_vaddr_t address) const {
  return safemath::CheckDiv<pgoff_t>(address, kPageSize).ValueOrDie();
}

zx::result<zx_vaddr_t> VmoMapping::GetAddress(pgoff_t offset) const {
  ZX_DEBUG_ASSERT(offset < get_size());
  if (!address()) {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }
  return zx::ok(safemath::CheckAdd<zx_vaddr_t>(address(), page_to_address(offset)).ValueOrDie());
}

VmoPaged::VmoPaged(zx::vmo &vmo, pgoff_t index, size_t size, bool zero)
    : VmoMapping(vmo, index, size, zero) {}

VmoPaged::~VmoPaged() {}

zx::result<bool> VmoPaged::Lock(pgoff_t offset) {
  increase_active_pages();
  return zx::ok(true);
}

zx_status_t VmoPaged::Unlock(pgoff_t offset) {
  decrease_active_pages();
  return ZX_OK;
}

zx_status_t VmoPaged::Zero(pgoff_t start, pgoff_t len) {
  ZX_DEBUG_ASSERT(start + len <= get_size());
  return vmo().op_range(ZX_VMO_OP_ZERO, page_to_address(start + index()), page_to_address(len),
                        nullptr, 0);
}

VmoDiscardable::VmoDiscardable(zx::vmo &vmo, pgoff_t index, size_t size, bool zero)
    : VmoMapping(vmo, index, size, zero) {
  page_bitmap_.resize(get_size(), false);
}

VmoDiscardable::~VmoDiscardable() {}

zx_status_t VmoDiscardable::Zero(pgoff_t start, pgoff_t len) {
  ZX_DEBUG_ASSERT(start + len <= get_size());
  return vmo().op_range(ZX_VMO_OP_ZERO, page_to_address(start), page_to_address(len), nullptr, 0);
}

zx::result<bool> VmoDiscardable::Lock(pgoff_t offset) {
  if (!active_pages()) {
    auto size = page_to_address(get_size());
    zx_status_t status = vmo().op_range(ZX_VMO_OP_TRY_LOCK, 0, size, nullptr, 0);
    if (status == ZX_ERR_UNAVAILABLE) {
      // When kernel has decommitted any Pages in |vmo_|, the corresponding bits in |page_bitmap_|
      // are cleared too.
      zx_vmo_lock_state_t lock_state;
      status = vmo().op_range(ZX_VMO_OP_LOCK, 0, size, &lock_state, sizeof(lock_state));
      uint64_t discarded_offset = address_to_page(lock_state.discarded_offset);
      uint64_t end_offset =
          safemath::CheckAdd<uint64_t>(lock_state.discarded_offset, lock_state.discarded_size)
              .ValueOrDie();
      end_offset = address_to_page(fbl::round_up(end_offset, kPageSize));
      for (; discarded_offset < end_offset; ++discarded_offset) {
        if (page_bitmap_[discarded_offset]) {
          page_bitmap_[discarded_offset] = false;
          decrease_active_pages();
        }
      }
    }
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  size_t offset_in_bitmap = offset % get_size();
  bool committed = page_bitmap_[offset_in_bitmap];
  if (!committed) {
    page_bitmap_[offset_in_bitmap] = true;
    increase_active_pages();
  }
  return zx::ok(committed);
}

zx_status_t VmoDiscardable::Unlock(pgoff_t offset) {
  size_t offset_in_bitmap = offset % get_size();
  if (page_bitmap_[offset_in_bitmap]) {
    page_bitmap_[offset_in_bitmap] = false;
    if (decrease_active_pages()) {
      return ZX_OK;
    }
  }
  return vmo().op_range(ZX_VMO_OP_UNLOCK, 0, page_to_address(get_size()), nullptr, 0);
}

zx::result<bool> VmoManager::CreateAndLockVmo(const pgoff_t index) __TA_EXCLUDES(mutex_) {
  std::lock_guard tree_lock(mutex_);
  bool zero = false;
  if (index >= size_in_blocks_) {
    if (vmo_.is_valid()) {
      // extend paged vmo
      // TODO: consider updating the content size
      // TODO: consider the shrinking case
      auto new_size = fbl::round_up(index + 1, node_size_in_blocks_) * kBlockSize;
      auto status = vmo_.set_size(new_size);
      ZX_ASSERT_MSG(status == ZX_OK, "failed to resize paged vmo (%lu > %lu): %s",
                    size_in_blocks_ * kBlockSize, new_size, zx_status_get_string(status));
      size_in_blocks_ = new_size / kBlockSize;
    } else {
      size_in_blocks_ = fbl::round_up(index + 1, node_size_in_blocks_);
    }
    // zero the appended area
    zero = true;
  }
  auto vmo_node_or = GetVmoNodeUnsafe(GetVmoNodeKey(index), zero);
  ZX_DEBUG_ASSERT(vmo_node_or.is_ok());
  return vmo_node_or.value()->Lock(index);
}

zx_status_t VmoManager::UnlockVmo(const pgoff_t index, const bool evict) {
  if (evict) {
    fs::SharedLock tree_lock(mutex_);
    ZX_ASSERT(index < size_in_blocks_);
    auto vmo_node_or = FindVmoNodeUnsafe(GetVmoNodeKey(index));
    if (vmo_node_or.is_ok()) {
      if (auto status = vmo_node_or.value()->Unlock(index); status != ZX_OK) {
        return status;
      }
      // TODO: consider removing a vmo_node when all regarding pages are invalidated.
    }
    return vmo_node_or.status_value();
  }
  return ZX_OK;
}

zx::result<zx_vaddr_t> VmoManager::GetAddress(pgoff_t index) {
  fs::SharedLock tree_lock(mutex_);
  ZX_ASSERT(index < size_in_blocks_);
  auto vmo_node_or = FindVmoNodeUnsafe(GetVmoNodeKey(index));
  if (vmo_node_or.is_ok()) {
    return vmo_node_or.value()->GetAddress(GetOffsetInVmoNode(index));
  }
  return zx::error(vmo_node_or.error_value());
}

void VmoManager::Reset(bool shutdown) {
  std::lock_guard tree_lock(mutex_);
  pgoff_t prev_key = std::numeric_limits<pgoff_t>::max();
  while (!vmo_tree_.is_empty()) {
    if (shutdown) {
      [[maybe_unused]] auto evicted = vmo_tree_.pop_front();
    } else {
      auto key = (prev_key < std::numeric_limits<pgoff_t>::max()) ? prev_key : 0;
      auto current = vmo_tree_.lower_bound(key);
      if (current == vmo_tree_.end()) {
        break;
      }
      // Unless the |prev_key| Page is evicted, try the next Page.
      if (prev_key == current->GetKey()) {
        ++current;
        if (current == vmo_tree_.end()) {
          break;
        }
      }

      prev_key = current->GetKey();
      if (!current->GetActivePages()) {
        [[maybe_unused]] auto evicted = vmo_tree_.erase(*current);
      }
    }
  }
}

zx::result<VmoMapping *> VmoManager::FindVmoNodeUnsafe(const pgoff_t index) {
  if (auto vmo_node = vmo_tree_.find(index); vmo_node != vmo_tree_.end()) {
    return zx::ok(&(*vmo_node));
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<VmoMapping *> VmoManager::GetVmoNodeUnsafe(const pgoff_t index, bool zero) {
  VmoMapping *vmo_node = nullptr;
  if (auto vmo_node_or = FindVmoNodeUnsafe(index); vmo_node_or.is_error()) {
    std::unique_ptr<VmoMapping> new_node;
    if (mode_ == VmoMode::kDiscardable) {
      new_node = std::make_unique<VmoDiscardable>(vmo_, index, node_size_in_blocks_, zero);
    } else {
      new_node = std::make_unique<VmoPaged>(vmo_, index, node_size_in_blocks_, zero);
    }
    vmo_node = new_node.get();
    vmo_tree_.insert(std::move(new_node));
  } else {
    vmo_node = vmo_node_or.value();
  }
  return zx::ok(vmo_node);
}

zx_status_t VmoManager::ZeroRange(pgoff_t start, pgoff_t end) {
  fs::SharedLock lock(mutex_);
  end = std::min(end, size_in_blocks_);
  if (start < end && end <= size_in_blocks_) {
    if (vmo_.is_valid()) {
      return vmo_.op_range(ZX_VMO_OP_ZERO, start * kBlockSize, (end - start) * kBlockSize, nullptr,
                           0);
    } else {
      auto end_node = fbl::round_up(end, node_size_in_blocks_);
      for (auto i = start; i < end_node; i = GetVmoNodeKey(i + node_size_in_blocks_)) {
        if (auto vmo_node_or = FindVmoNodeUnsafe(GetVmoNodeKey(i)); vmo_node_or.is_ok()) {
          auto len = std::min(end - i, node_size_in_blocks_ - GetOffsetInVmoNode(i));
          ZX_ASSERT(vmo_node_or->Zero(GetOffsetInVmoNode(i), len) == ZX_OK);
        }
      }
    }
  }
  return ZX_OK;
}

zx::result<size_t> VmoManager::WritebackBegin(PagedVfsCallback cb) {
  zx_status_t error_status = ZX_ERR_NOT_SUPPORTED;
  std::lock_guard lock(mutex_);
  if (size_in_blocks_) {
    auto ret = cb(vmo_, 0, size_in_blocks_ * kBlockSize);
    if (ret.is_ok()) {
      ++writeback_ops_;
      return zx::ok(size_in_blocks_);
    }
    error_status = ret.error_value();
  }
  FX_LOGS(WARNING) << "ZX_PAGER_OP_WRITEBACK_BEGIN failed. " << zx_status_get_string(error_status);
  return zx::error(error_status);
}

zx_status_t VmoManager::WritebackEnd(PagedVfsCallback cb, size_t size_in_blocks) {
  zx_status_t error_status = ZX_ERR_NOT_SUPPORTED;
  std::lock_guard lock(mutex_);
  if (size_in_blocks) {
    auto status = cb(vmo_, 0, size_in_blocks * kBlockSize);
    if (status.is_ok()) {
      --writeback_ops_;
    } else {
      FX_LOGS(WARNING) << "ZX_PAGER_OP_WRITEBACK_END failed. " << status.status_string();
    }
    error_status = status.status_value();
  }
  return error_status;
}

pgoff_t VmoManager::GetOffsetInVmoNode(pgoff_t page_index) const {
  return page_index % node_size_in_blocks_;
}

pgoff_t VmoManager::GetVmoNodeKey(pgoff_t page_index) const {
  return fbl::round_down(page_index, node_size_in_blocks_);
}

}  // namespace f2fs
