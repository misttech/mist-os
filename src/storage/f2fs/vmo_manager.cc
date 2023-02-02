// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {
namespace {

zx_vaddr_t page_to_address(pgoff_t page_index) {
  return safemath::CheckMul<zx_vaddr_t>(page_index, kPageSize).ValueOrDie();
}
pgoff_t address_to_page(zx_vaddr_t address) {
  return safemath::CheckDiv<pgoff_t>(address, kPageSize).ValueOrDie();
}

}  // namespace

VmoMapping::VmoMapping(zx::vmo &unowned_vmo, pgoff_t index, size_t size, bool zero)
    : size_in_blocks_(size), index_(index), vmo_(unowned_vmo) {
  zx_vaddr_t addr;
  if (!unowned_vmo.is_valid()) {
    ZX_ASSERT(zx::vmo::create(page_to_address(get_size()), ZX_VMO_DISCARDABLE, &owned_vmo_) ==
              ZX_OK);
    ZX_ASSERT(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_ALLOW_FAULTS, 0,
                                         vmo(), 0, page_to_address(get_size()), &addr) == ZX_OK);
  } else {
    // Once F2fs supports fs::StreamFileConnection, it might not use vaddr anymore for file vnodes
    // as it doesn't need to touch the content of files.
    ZX_ASSERT(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_ALLOW_FAULTS, 0,
                                         vmo(), page_to_address(index), page_to_address(get_size()),
                                         &addr) == ZX_OK);
  }
  address_ = addr;
}

VmoMapping::~VmoMapping() {
  if (address()) {
    ZX_ASSERT(zx::vmar::root_self()->unmap(address(), page_to_address(get_size())) == ZX_OK);
  }
}

zx::result<zx_vaddr_t> VmoMapping::GetAddress(pgoff_t offset) const {
  ZX_DEBUG_ASSERT(offset < get_size());
  if (unlikely(!address())) {
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

VmoManager::VmoManager(VmoMode mode, size_t content_size, size_t node_size, zx::vmo vmo)
    : mode_(mode), node_size_in_blocks_(address_to_page(node_size)), vmo_(std::move(vmo)) {
  ZX_ASSERT(!(node_size % kBlockSize));
  std::lock_guard lock(mutex_);
  if (vmo_.is_valid()) {
    size_t vmo_size;
    ZX_ASSERT(vmo_.get_size(&vmo_size) == ZX_OK);
    size_t new_size = fbl::round_up(std::max(content_size, vmo_size), node_size);
    if (new_size > vmo_size) {
      vmo_size = new_size;
      ZX_ASSERT(vmo_.set_size(vmo_size) == ZX_OK);
    }
    size_in_blocks_ = address_to_page(vmo_size);
  }
}

zx::result<bool> VmoManager::CreateAndLockVmo(const pgoff_t index) __TA_EXCLUDES(mutex_) {
  std::lock_guard tree_lock(mutex_);
  bool zero = false;
  if (index >= size_in_blocks_) {
    if (vmo_.is_valid()) {
      // extend paged vmo
      auto new_size = page_to_address(fbl::round_up(index + 1, node_size_in_blocks_));
      auto status = vmo_.set_size(new_size);
      ZX_ASSERT_MSG(status == ZX_OK, "failed to resize paged vmo (%lu > %lu): %s",
                    page_to_address(size_in_blocks_), new_size, zx_status_get_string(status));
      size_in_blocks_ = address_to_page(new_size);
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
      // TODO(https://fxbug.dev/119885): consider removing a vmo_node when all regarding pages are
      // invalidated.
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
  auto current = vmo_tree_.begin();
  while (!vmo_tree_.is_empty() && current != vmo_tree_.end()) {
    auto node = current;
    ++current;
    if (shutdown || !node->GetActivePages()) {
      vmo_tree_.erase(*node);
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

void VmoManager::ZeroBlocks(fs::PagedVfs &vfs, pgoff_t start, pgoff_t end) {
  std::lock_guard lock(mutex_);
  if (unlikely(start == size_in_blocks_ || start >= end)) {
    return;
  }
  if (vmo_.is_valid()) {
    size_t offset = page_to_address(start);
    if (start > size_in_blocks_) {
      // Extent the size of |vmo_| for truncate() to a larger size.
      auto new_size = fbl::round_up(start, node_size_in_blocks_);
      vmo_.set_size(page_to_address(new_size));
      size_in_blocks_ = new_size;
    } else if (start < size_in_blocks_ && end >= size_in_blocks_) {
      // Shrink the content size of |vmo_| for truncate() to a smaller size.
      vmo_.set_prop_content_size(offset);
    } else {
      // For punch-a-hole operations, it zeroes the area.
      // Make the target area dirty first, and do ZX_VMO_OP_ZERO.
      size_t size = page_to_address(end - start);
      auto end_or = DirtyPagesUnsafe(vfs, offset, offset + size);
      ZX_ASSERT(end_or.is_ok() || end_or.status_value() == ZX_ERR_NOT_FOUND);
      if (auto ret = vmo_.op_range(ZX_VMO_OP_ZERO, offset, size, nullptr, 0); ret != ZX_OK) {
        FX_LOGS(WARNING) << "failed to do ZX_PAGER_OP_ZERO. " << zx_status_get_string(ret);
        return;
      }
      // Makes all dirty pages in the range cleared. So, any further changes in this range
      // triggers Vnode::VmoDirty().
      ClearDirtyPagesUnsafe(vfs, offset, offset + size);
    }
  } else {
    end = std::min(end, size_in_blocks_);
    if (start < end) {
      auto end_node = fbl::round_up(end, node_size_in_blocks_);
      for (auto i = start; i < end_node; i = GetVmoNodeKey(i + node_size_in_blocks_)) {
        if (auto vmo_node_or = FindVmoNodeUnsafe(GetVmoNodeKey(i)); vmo_node_or.is_ok()) {
          auto len = std::min(end - i, node_size_in_blocks_ - GetOffsetInVmoNode(i));
          ZX_ASSERT(vmo_node_or->Zero(GetOffsetInVmoNode(i), len) == ZX_OK);
        }
      }
    }
  }
}

zx::result<size_t> VmoManager::WritebackBeginUnsafe(fs::PagedVfs &vfs, const size_t start,
                                                    const size_t end) {
  size_t actual_size =
      safemath::CheckSub(std::min(end, page_to_address(size_in_blocks_)), start).ValueOrDie();
  auto ret = vfs.WritebackBegin(vmo_, start, actual_size);
  if (unlikely(ret.is_error())) {
    FX_LOGS(WARNING) << "failed to do ZX_PAGER_OP_WRITEBACK_BEGIN. " << ret.status_string();
    return ret.take_error();
  }
  return zx::ok(start + actual_size);
}

zx::result<size_t> VmoManager::WritebackBegin(fs::PagedVfs &vfs, const size_t start,
                                              const size_t end) {
  if (unlikely(mode_ != VmoMode::kPaged)) {
    return zx::ok(0);
  }
  std::lock_guard lock(mutex_);
  auto ret = WritebackBeginUnsafe(vfs, start, end);
  if (ret.is_ok()) {
    --writeback_ops_;
  }
  return ret;
}

zx_status_t VmoManager::WritebackEndUnsafe(fs::PagedVfs &vfs, const size_t start,
                                           const size_t end) {
  auto status = vfs.WritebackEnd(vmo_, start, end - start);
  if (unlikely(status.is_error())) {
    FX_LOGS(WARNING) << "failed to do ZX_PAGER_OP_WRITEBACK_END. " << status.status_string();
    return status.error_value();
  }
  return ZX_OK;
}

zx_status_t VmoManager::WritebackEnd(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  std::lock_guard lock(mutex_);
  auto ret = WritebackEndUnsafe(vfs, start, end);
  if (ret == ZX_OK) {
    ++writeback_ops_;
  }
  return ret;
}

zx::result<size_t> VmoManager::DirtyPages(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  fs::SharedLock lock(mutex_);
  return DirtyPagesUnsafe(vfs, start, end);
}

zx::result<size_t> VmoManager::DirtyPagesUnsafe(fs::PagedVfs &vfs, const size_t start,
                                                const size_t end) {
  if (unlikely(mode_ != VmoMode::kPaged)) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  size_t actual_size =
      safemath::CheckSub(std::min(end, page_to_address(size_in_blocks_)), start).ValueOrDie();
  auto status = vfs.DirtyPages(vmo_, start, actual_size);
  if (status.is_error()) {
    return status.take_error();
  }
  return zx::ok(start + actual_size);
}

void VmoManager::ClearDirtyPages(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  if (mode_ == VmoMode::kPaged) {
    fs::SharedLock lock(mutex_);
    ClearDirtyPagesUnsafe(vfs, start, end);
  }
}

void VmoManager::ClearDirtyPagesUnsafe(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  if (mode_ == VmoMode::kPaged) {
    auto end_or = WritebackBeginUnsafe(vfs, start, end);
    if (end_or.is_ok()) {
      ZX_ASSERT(WritebackEndUnsafe(vfs, start, *end_or) == ZX_OK);
    }
  }
}

zx::result<> VmoManager::SupplyPages(fs::PagedVfs &vfs, const size_t offset, const size_t length,
                                     zx::vmo vmo, const size_t aux_offset) {
  fs::SharedLock lock(mutex_);
  auto ret = vfs.SupplyPages(vmo_, offset, length, std::move(vmo), aux_offset);
  if (unlikely(ret.is_error())) {
    FX_LOGS(ERROR) << "failed to supply vmo to paged_vmo " << ret.status_string();
    return ret.take_error();
  }
  return zx::ok();
}

pgoff_t VmoManager::GetOffsetInVmoNode(pgoff_t page_index) const {
  return page_index % node_size_in_blocks_;
}

pgoff_t VmoManager::GetVmoNodeKey(pgoff_t page_index) const {
  return fbl::round_down(page_index, node_size_in_blocks_);
}

VmoCleaner::VmoCleaner(VnodeF2fs &vnode, const pgoff_t start, const pgoff_t end)
    : vfs_(*vnode.fs()->vfs()), manager_(vnode.GetVmoManager()), offset_(page_to_address(start)) {
  ZX_ASSERT(start < end);
  size_t end_offset = end;
  if (end < kPgOffMax) {
    end_offset = page_to_address(end);
  }
  auto size_or = manager_.WritebackBegin(vfs_, offset_, end_offset);
  if (size_or.is_ok()) {
    size_ = *size_or;
  }
}

VmoCleaner::~VmoCleaner() {
  if (size_) {
    ZX_ASSERT(manager_.WritebackEnd(vfs_, offset_, size_) == ZX_OK);
  }
}

}  // namespace f2fs
