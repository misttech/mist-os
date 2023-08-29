// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

StorageBuffer::StorageBuffer(Bcache *bc, size_t blocks, uint32_t block_size, std::string_view label,
                             uint32_t allocation_unit)
    : max_blocks_(bc->Maxblk()), allocation_unit_(allocation_unit) {
  ZX_DEBUG_ASSERT(allocation_unit >= 1 && allocation_unit <= blocks);
  blocks = fbl::round_up(blocks, allocation_unit);
  ZX_ASSERT(buffer_.Initialize(bc, blocks, block_size, label.data()) == ZX_OK);
  Init();
}

void StorageBuffer::Init() {
  std::lock_guard lock(mutex_);
  for (size_t i = 0; i < buffer_.capacity(); i += allocation_unit_) {
    auto key = std::make_unique<VmoBufferKey>(i);
    free_keys_.push_back(std::move(key));
  }
}

zx::result<size_t> StorageBuffer::ReserveWriteOperation(Page &page) {
  ZX_DEBUG_ASSERT(page.GetBlockAddr() != kNullAddr);
  if (page.GetBlockAddr() >= max_blocks_) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }
  std::lock_guard lock(mutex_);
  // No room in |buffer_|.
  if (free_keys_.is_empty()) {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }

  auto key = free_keys_.pop_front();
  storage::Operation op = {
      .type = storage::OperationType::kWrite,
      .vmo_offset = key->GetKey(),
      .dev_offset = page.GetBlockAddr(),
      .length = 1,
  };
  // Copy |page| to |buffer| at |key|.
  page.Read(buffer_.Data(op.vmo_offset));
  // Here, |operation| can be merged into a previous operation.
  builder_.Add(op, &buffer_);
  reserved_keys_.push_back(std::move(key));
  return zx::ok(reserved_keys_.size());
}

zx::result<StorageOperations> StorageBuffer::MakeReadOperations(const std::vector<block_t> &addrs) {
  VmoKeyList keys;
  uint32_t allocate_index = 0;
  fs::BufferedOperationsBuilder builder;
  std::lock_guard lock(mutex_);
  for (auto addr : addrs) {
    if (addr != kNullAddr && addr != kNewAddr) {
      // If addr is invalid, free allocated keys.
      if (addr >= max_blocks_) {
        free_keys_.splice(free_keys_.end(), keys);
        return zx::error(ZX_ERR_OUT_OF_RANGE);
      }

      if (allocate_index % allocation_unit_ == 0) {
        allocate_index = 0;
        // Wait until there is a room in |buffer_|.
        while (free_keys_.is_empty()) {
          if (auto wait_result = cvar_.wait_for(mutex_, std::chrono::seconds(kWriteTimeOut));
              wait_result == std::cv_status::timeout) {
            return zx::error(ZX_ERR_TIMED_OUT);
          }
        }
        keys.push_back(free_keys_.pop_front());
      }

      storage::Operation op = {
          .type = storage::OperationType::kRead,
          .vmo_offset = keys.back().GetKey() + allocate_index,
          .dev_offset = addr,
          .length = 1,
      };
      builder.Add(op, &buffer_);
      ++allocate_index;
    }
  }
  return zx::ok(StorageOperations(*this, builder, keys));
}

void StorageBuffer::ReleaseBuffers(const StorageOperations &operation) {
  if (!operation.IsEmpty()) {
    auto keys = operation.TakeVmoKeys();
    std::lock_guard lock(mutex_);
    ZX_DEBUG_ASSERT(!keys.is_empty());
    // Add vmo buffers of |operation| to |free_keys_| to allow waiters to reserve buffer_.
    free_keys_.splice(free_keys_.end(), keys);
    // TODO: When multi-qd is available, consider notify_all().
    cvar_.notify_all();
  }
}

StorageOperations StorageBuffer::TakeWriteOperations() {
  std::lock_guard lock(mutex_);
  return StorageOperations(*this, builder_, reserved_keys_);
}

StorageBuffer::~StorageBuffer() {
  {
    std::lock_guard lock(mutex_);
    [[maybe_unused]] size_t num_keys = 0;
    while (!free_keys_.is_empty()) {
      ++num_keys;
      free_keys_.pop_front();
    }
    ZX_DEBUG_ASSERT(num_keys == buffer_.capacity() / allocation_unit_);
  }
}

}  // namespace f2fs
