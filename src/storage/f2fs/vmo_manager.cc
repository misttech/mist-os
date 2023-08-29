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

VmoMapping::VmoMapping(pgoff_t index, size_t size) : size_in_blocks_(size), index_(index) {
  zx_vaddr_t addr;
  ZX_ASSERT(zx::vmo::create(page_to_address(get_size()), ZX_VMO_DISCARDABLE, &owned_vmo_) == ZX_OK);
  ZX_ASSERT(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_ALLOW_FAULTS, 0,
                                       vmo(), 0, page_to_address(get_size()), &addr) == ZX_OK);
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

VmoDiscardable::VmoDiscardable(pgoff_t index, size_t size) : VmoMapping(index, size) {
  page_bitmap_.resize(get_size(), false);
}

VmoDiscardable::~VmoDiscardable() {}

zx_status_t VmoDiscardable::Zero(pgoff_t start, pgoff_t len) {
  ZX_DEBUG_ASSERT(start + len <= get_size());
  return vmo().op_range(ZX_VMO_OP_ZERO, page_to_address(start), page_to_address(len), nullptr, 0);
}

zx::result<bool> VmoDiscardable::Lock(pgoff_t offset) {
  ZX_ASSERT(offset < get_size());
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

  bool committed = page_bitmap_[offset];
  if (!committed) {
    page_bitmap_[offset] = true;
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
    : mode_(mode),
      content_size_(content_size),
      node_size_in_blocks_(address_to_page(node_size)),
      node_size_(node_size),
      vmo_(std::move(vmo)) {
  ZX_ASSERT(!(node_size % kBlockSize));
  std::lock_guard lock(mutex_);
  if (mode == VmoMode::kPaged) {
    size_t vmo_size = 0;
    size_t vmo_content_size = 0;
    ZX_ASSERT(vmo_.is_valid());
    ZX_ASSERT(vmo_.get_size(&vmo_size) == ZX_OK);
    ZX_ASSERT(content_size <= vmo_size);
    ZX_ASSERT(vmo_.get_prop_content_size(&vmo_content_size) == ZX_OK);
    size_in_blocks_ = address_to_page(vmo_size);
    if (content_size != vmo_content_size) {
      ZX_ASSERT(vmo_.set_prop_content_size(content_size) == ZX_OK);
    }
  }
}

zx::result<bool> VmoManager::CreateAndLockVmo(const pgoff_t index, void **out)
    __TA_EXCLUDES(mutex_) {
  if (mode_ == VmoMode::kPaged) {
    return zx::ok(false);
  }
  std::lock_guard tree_lock(mutex_);
  if (index >= size_in_blocks_) {
    size_in_blocks_ = fbl::round_up(index + 1, node_size_in_blocks_);
  }
  auto vmo_node_or = GetVmoNodeUnsafe(GetVmoNodeKey(index));
  ZX_DEBUG_ASSERT(vmo_node_or.is_ok());
  if (out) {
    auto addr_or = vmo_node_or.value()->GetAddress(GetOffsetInVmoNode(index));
    if (addr_or.is_ok()) {
      *out = reinterpret_cast<void *>(*addr_or);
    }
  }
  return vmo_node_or.value()->Lock(GetOffsetInVmoNode(index));
}

zx_status_t VmoManager::UnlockVmo(const pgoff_t index, const bool evict) {
  if (mode_ != VmoMode::kDiscardable) {
    return ZX_OK;
  }
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

zx::result<VmoMapping *> VmoManager::GetVmoNodeUnsafe(const pgoff_t index) {
  if (mode_ != VmoMode::kDiscardable) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  VmoMapping *vmo_node = nullptr;
  if (auto vmo_node_or = FindVmoNodeUnsafe(index); vmo_node_or.is_error()) {
    std::unique_ptr<VmoMapping> new_node;
    new_node = std::make_unique<VmoDiscardable>(index, node_size_in_blocks_);
    vmo_node = new_node.get();
    vmo_tree_.insert(std::move(new_node));
  } else {
    vmo_node = vmo_node_or.value();
  }
  return zx::ok(vmo_node);
}

void VmoManager::ZeroBlocks(fs::PagedVfs &vfs, pgoff_t start, pgoff_t end) {
  fs::SharedLock lock(mutex_);
  if (unlikely(start >= size_in_blocks_ || start >= end)) {
    return;
  }
  if (mode_ == VmoMode::kPaged) {
    // For punch-a-hole operations, it zeroes the area.
    if (page_to_address(end) < GetContentSizeUnsafe(true)) {
      for (pgoff_t i = start; i < end; i += node_size_in_blocks_) {
        size_t offset = page_to_address(i);
        auto ret = DirtyPagesUnsafe(vfs, offset, offset + node_size_);
        // If the vmo at |i| is not supplied, do nothing.
        if (ret.is_ok()) {
          ZX_ASSERT(vmo_.op_range(ZX_VMO_OP_ZERO, offset, node_size_, nullptr, 0) == ZX_OK);
        }
      }
    }
  } else {
    end = std::min(end, size_in_blocks_);
    if (start < end) {
      pgoff_t end_node = fbl::round_up(end, node_size_in_blocks_);
      for (pgoff_t i = start; i < end_node; i = GetVmoNodeKey(i + node_size_in_blocks_)) {
        if (auto vmo_node_or = FindVmoNodeUnsafe(GetVmoNodeKey(i)); vmo_node_or.is_ok()) {
          size_t len = std::min(end - i, node_size_in_blocks_ - GetOffsetInVmoNode(i));
          ZX_ASSERT(vmo_node_or->Zero(GetOffsetInVmoNode(i), len) == ZX_OK);
        }
      }
    }
  }
}

zx::result<> VmoManager::WritebackBeginUnsafe(fs::PagedVfs &vfs, const size_t start,
                                              const size_t end) {
  auto ret = vfs.WritebackBegin(vmo_, start, end - start);
  if (unlikely(ret.is_error())) {
    FX_LOGS(WARNING) << "failed to do ZX_PAGER_OP_WRITEBACK_BEGIN. " << ret.status_string();
    return ret.take_error();
  }
  return zx::ok();
}

zx::result<> VmoManager::WritebackBegin(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  if (unlikely(mode_ != VmoMode::kPaged)) {
    return zx::ok();
  }
  fs::SharedLock lock(mutex_);
  return WritebackBeginUnsafe(vfs, start, end);
}

zx_status_t VmoManager::WritebackEndUnsafe(fs::PagedVfs &vfs, const size_t start,
                                           const size_t end) {
  if (start >= end) {
    return ZX_OK;
  }
  auto status = vfs.WritebackEnd(vmo_, start, end - start);
  if (unlikely(status.is_error())) {
    FX_LOGS(WARNING) << "failed to do ZX_PAGER_OP_WRITEBACK_END. " << status.status_string();
  }
  return status.status_value();
}

zx_status_t VmoManager::WritebackEnd(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  if (unlikely(mode_ != VmoMode::kPaged)) {
    return ZX_OK;
  }
  fs::SharedLock lock(mutex_);
  return WritebackEndUnsafe(vfs, start, end);
}

zx::result<> VmoManager::DirtyPages(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  fs::SharedLock lock(mutex_);
  return DirtyPagesUnsafe(vfs, start, end);
}

zx::result<> VmoManager::DirtyPagesUnsafe(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  if (unlikely(mode_ != VmoMode::kPaged)) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  size_t actual_end = std::min(end, GetContentSizeUnsafe(true));
  // Someone has already truncated it.
  if (start >= actual_end) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  auto status = vfs.DirtyPages(vmo_, start, actual_end - start);
  if (status.is_error()) {
    return status.take_error();
  }
  return zx::ok();
}

void VmoManager::SetContentSize(const size_t nbytes) {
  std::lock_guard lock(mutex_);
  if (mode_ == VmoMode::kPaged) {
    UpdateSizeUnsafe();
    size_t content_size = GetContentSizeUnsafe(false);
    if (nbytes > page_to_address(size_in_blocks_)) {
      // Extent the size of |vmo_| to a larger size.
      size_in_blocks_ = address_to_page(fbl::round_up(nbytes, node_size_));
      vmo_.set_size(nbytes);
    } else if (nbytes != content_size) {
      // Adjust the content size of |vmo_|.
      ZX_ASSERT(nbytes <= page_to_address(size_in_blocks_));
      vmo_.set_prop_content_size(nbytes);
    }
  } else {
    content_size_ = nbytes;
  }
}

uint64_t VmoManager::GetContentSizeUnsafe(bool round_up) {
  if (mode_ == VmoMode::kPaged) {
    ZX_ASSERT(vmo_.get_prop_content_size(&content_size_) == ZX_OK);
  }
  if (round_up) {
    return fbl::round_up(content_size_, node_size_);
  }
  return content_size_;
}

uint64_t VmoManager::GetContentSize(bool round_up) {
  fs::SharedLock lock(mutex_);
  return GetContentSizeUnsafe(round_up);
}

void VmoManager::UpdateSizeUnsafe() {
  if (mode_ != VmoMode::kPaged) {
    return;
  }
  size_t vmo_size;
  vmo_.get_size(&vmo_size);
  size_in_blocks_ = address_to_page(fbl::round_up(vmo_size, page_to_address(node_size_in_blocks_)));
}

void VmoManager::AllowEviction(fs::PagedVfs &vfs, const size_t start, const size_t end) {
  if (mode_ != VmoMode::kPaged) {
    return;
  }
  fs::SharedLock lock(mutex_);
  size_t actual_end = std::min(end, GetContentSizeUnsafe(true));
  if (start < actual_end) {
    ZX_ASSERT(vmo_.op_range(ZX_VMO_OP_DONT_NEED, start, actual_end - start, nullptr, 0) == ZX_OK);
  }
}

zx_status_t VmoManager::Read(void *data, uint64_t offset, size_t len) {
  if (mode_ != VmoMode::kPaged) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  fs::SharedLock lock(mutex_);
  return vmo_.read(data, offset, len);
}

zx_status_t VmoManager::Write(const void *data, uint64_t offset, size_t len) {
  if (mode_ != VmoMode::kPaged) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  fs::SharedLock lock(mutex_);
  return vmo_.write(data, offset, len);
}

pgoff_t VmoManager::GetOffsetInVmoNode(pgoff_t page_index) const {
  return page_index % node_size_in_blocks_;
}

pgoff_t VmoManager::GetVmoNodeKey(pgoff_t page_index) const {
  return fbl::round_down(page_index, node_size_in_blocks_);
}

zx_status_t VmoHolder::Read(void *data, uint64_t offset, size_t len) {
  return manager_.Read(data, page_to_address(index_) + offset, len);
}

zx_status_t VmoHolder::Write(const void *data, uint64_t offset, size_t len) {
  return manager_.Write(data, page_to_address(index_) + offset, len);
}

VmoCleaner::VmoCleaner(bool bSync, fbl::RefPtr<VnodeF2fs> vnode, const pgoff_t start,
                       const pgoff_t end)
    : vnode_(std::move(vnode)), sync_(bSync), offset_(page_to_address(start)) {
  ZX_ASSERT(start < end);
  size_t end_offset = end;
  if (end < kPgOffMax) {
    end_offset = page_to_address(end);
  }
  end_offset = std::min(end, vnode_->GetVmoManager().GetContentSize(true));
  if (start >= end_offset) {
    return;
  }
  end_offset_ = end_offset;
  if (sync_) {
    ZX_ASSERT(
        vnode_->GetVmoManager().WritebackBegin(*vnode_->fs()->vfs(), offset_, end_offset_).is_ok());
  } else {
    auto wb_begin_task = fpromise::make_promise([vnode = vnode_, offset = offset_,
                                                 end_offset = end_offset_]() {
      ZX_ASSERT(
          vnode->GetVmoManager().WritebackBegin(*vnode->fs()->vfs(), offset, end_offset).is_ok());
    });
    vnode_->fs()->ScheduleWriter(std::move(wb_begin_task));
  }
}

VmoCleaner::~VmoCleaner() {
  if (!end_offset_) {
    return;
  }
  if (sync_) {
    ZX_ASSERT(vnode_->GetVmoManager().WritebackEnd(*vnode_->fs()->vfs(), offset_, end_offset_) ==
              ZX_OK);
  } else {
    auto wb_end_task =
        fpromise::make_promise([vnode = vnode_, offset = offset_, end_offset = end_offset_]() {
          ZX_ASSERT(vnode->GetVmoManager().WritebackEnd(*vnode->fs()->vfs(), offset, end_offset) ==
                    ZX_OK);
        });
    vnode_->fs()->ScheduleWriter(std::move(wb_end_task));
  }
}

}  // namespace f2fs
