// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

VmoMapping::VmoMapping(zx::vmo &unowned_vmo, pgoff_t index, size_t size)
    : size_in_blocks_(size), index_(index), vmo_(unowned_vmo) {
  zx_vaddr_t addr;
  if (!unowned_vmo.is_valid()) {
    ZX_ASSERT(zx::vmo::create(page_to_address(get_size()), ZX_VMO_DISCARDABLE, &owned_vmo_) ==
              ZX_OK);
    ZX_ASSERT(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_ALLOW_FAULTS, 0,
                                         vmo(), 0, page_to_address(get_size()), &addr) == ZX_OK);
  } else {
    ZX_ASSERT(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_ALLOW_FAULTS, 0,
                                         vmo(), page_to_address(index), page_to_address(get_size()),
                                         &addr) == ZX_OK);
  }
  address_ = addr;
}

VmoMapping::~VmoMapping() {
  ZX_DEBUG_ASSERT(!active_pages());
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

void VmoMapping::Zero(pgoff_t offset) {
  ZX_DEBUG_ASSERT(offset < get_size());
  std::memset(reinterpret_cast<uint8_t *>(address()) + page_to_address(offset), 0, kBlockSize);
}

zx::result<zx_vaddr_t> VmoMapping::GetAddress(pgoff_t offset) const {
  ZX_DEBUG_ASSERT(offset < get_size());
  if (!address()) {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }
  return zx::ok(safemath::CheckAdd<zx_vaddr_t>(address(), page_to_address(offset)).ValueOrDie());
}

VmoPaged::VmoPaged(zx::vmo &vmo, pgoff_t index, size_t size) : VmoMapping(vmo, index, size) {}

VmoPaged::~VmoPaged() {}

zx::result<bool> VmoPaged::Lock(pgoff_t offset) {
  increase_active_pages();
  return zx::ok(true);
}

zx_status_t VmoPaged::Unlock(pgoff_t offset) {
  decrease_active_pages();
  return ZX_OK;
}

VmoNode::VmoNode(zx::vmo &vmo, pgoff_t index, size_t size) : VmoMapping(vmo, index, size) {
  page_bitmap_.resize(get_size(), false);
}

VmoNode::~VmoNode() {}

zx::result<bool> VmoNode::Lock(pgoff_t offset) {
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
        page_bitmap_[discarded_offset] = false;
      }
    }
    ZX_DEBUG_ASSERT(status == ZX_OK);
  }

  size_t offset_in_bitmap = offset % get_size();
  bool committed = page_bitmap_[offset_in_bitmap];
  if (!committed) {
    page_bitmap_[offset_in_bitmap] = true;
  }
  increase_active_pages();
  return zx::ok(committed);
}

zx_status_t VmoNode::Unlock(pgoff_t offset) {
  if (decrease_active_pages()) {
    return ZX_OK;
  }
  return vmo().op_range(ZX_VMO_OP_UNLOCK, 0, page_to_address(get_size()), nullptr, 0);
}

zx::result<bool> VmoManager::CreateAndLockVmo(const pgoff_t index) __TA_EXCLUDES(mutex_) {
  std::lock_guard tree_lock(mutex_);
  if (vmo_.is_valid()) {
    if (index >= size_in_blocks_) {
      // extend paged vmo
      // TODO: consider updating the content size
      // TODO: consider the shrinking case
      auto new_size = fbl::round_up(index + 1, node_size_in_blocks_) * kBlockSize;
      auto status = vmo_.set_size(new_size);
      ZX_ASSERT_MSG(status == ZX_OK, "failed to resize paged vmo (%lu > %lu): %s",
                    size_in_blocks_ * kBlockSize, new_size, zx_status_get_string(status));
      size_in_blocks_ = new_size / kBlockSize;
    }
  } else {
    ZX_ASSERT(index < size_in_blocks_);
  }
  auto vmo_node_or = GetVmoNodeUnsafe(GetVmoNodeKey(index));
  ZX_DEBUG_ASSERT(vmo_node_or.is_ok());
  return vmo_node_or.value()->Lock(index);
}

zx_status_t VmoManager::UnlockVmo(const pgoff_t index, const bool evict) {
  std::lock_guard tree_lock(mutex_);
  ZX_ASSERT(index < size_in_blocks_);
  auto vmo_node_or = FindVmoNodeUnsafe(GetVmoNodeKey(index));
  if (vmo_node_or.is_ok()) {
    if (auto status = vmo_node_or.value()->Unlock(index); status != ZX_OK) {
      return status;
    }
    if (evict) {
      // TODO: consider using op_zero.
      // TODO: consider removing a vmo_node when all regarding pages are invalidated.
      vmo_node_or->Zero(GetOffsetInVmoNode(index));
    }
  }
  return vmo_node_or.status_value();
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

zx::result<VmoMapping *> VmoManager::GetVmoNodeUnsafe(const pgoff_t index) {
  VmoMapping *vmo_node = nullptr;
  if (auto vmo_node_or = FindVmoNodeUnsafe(index); vmo_node_or.is_error()) {
    std::unique_ptr<VmoMapping> new_node;
    if (mode_ == VmoMode::kDiscardable) {
      new_node = std::make_unique<VmoNode>(vmo_, index, node_size_in_blocks_);
    } else {
      new_node = std::make_unique<VmoPaged>(vmo_, index, node_size_in_blocks_);
    }
    vmo_node = new_node.get();
    vmo_tree_.insert(std::move(new_node));
  } else {
    vmo_node = vmo_node_or.value();
  }
  return zx::ok(vmo_node);
}

pgoff_t VmoManager::GetOffsetInVmoNode(pgoff_t page_index) const {
  return page_index % node_size_in_blocks_;
}

pgoff_t VmoManager::GetVmoNodeKey(pgoff_t page_index) const {
  return page_index - GetOffsetInVmoNode(page_index);
}

}  // namespace f2fs
