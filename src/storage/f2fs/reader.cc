// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

Reader::Reader(BcacheMapper *bc, size_t capacity) : bc_(bc) {
  buffer_ = std::make_unique<StorageBuffer>(bc, capacity, kBlockSize, "ReadBuffer",
                                            kDefaultAllocationUnit_);
}

zx::result<> Reader::ReadBlocks(zx::vmo &vmo, std::vector<block_t> &addrs) {
  auto operation_or = buffer_->MakeReadOperations(addrs);
  if (operation_or.is_error()) {
    FX_LOGS(ERROR) << "failed to make ReadOperations. "
                   << zx_status_get_string(operation_or.status_value());
    return operation_or.take_error();
  } else {
    // If every addr is set to either kNullAddr or kNewAddr, the size of
    // |*operations_or| is zero.
    if (auto status = RunIO(*operation_or,
                            [&](const StorageOperations &operation, zx_status_t io_status) {
                              if (io_status == ZX_OK) {
                                auto &keys = operation.VmoKeys();
                                auto key = keys.begin();
                                uint32_t allocate_index = 0;
                                for (size_t i = 0; i < addrs.size(); ++i) {
                                  if (addrs[i] != kNullAddr && addrs[i] != kNewAddr) {
                                    vmo.write(buffer_->Data(key->GetKey() + allocate_index),
                                              i * kBlockSize, kBlockSize);
                                    if ((++allocate_index) == kDefaultAllocationUnit_) {
                                      allocate_index = 0;
                                      ++key;
                                    }
                                  }
                                }
                              }
                            });
        status != ZX_OK) {
      return zx::error(status);
    }
  }
  return zx::ok();
}

zx::result<> Reader::ReadBlocks(std::vector<LockedPage> &pages, std::vector<block_t> &addrs) {
  auto operation_or = buffer_->MakeReadOperations(addrs);
  if (operation_or.is_error()) {
    FX_LOGS(ERROR) << "failed to make ReadOperations. "
                   << zx_status_get_string(operation_or.status_value());
    return operation_or.take_error();
  } else {
    // If every addr is set to either kNullAddr or kNewAddr, the size of
    // |*operations_or| is zero.
    if (auto status = RunIO(*operation_or,
                            [&](const StorageOperations &operation, zx_status_t io_status) {
                              if (io_status == ZX_OK) {
                                auto &keys = operation.VmoKeys();
                                auto key = keys.begin();
                                uint32_t allocate_index = 0;
                                for (size_t i = 0; i < addrs.size(); ++i) {
                                  if (addrs[i] == kNullAddr || pages[i]->IsUptodate()) {
                                    continue;
                                  }
                                  if (addrs[i] == kNewAddr) {
                                    pages[i].Zero();
                                  } else {
                                    pages[i]->Write(buffer_->Data(key->GetKey() + allocate_index));
                                    if ((++allocate_index) == kDefaultAllocationUnit_) {
                                      allocate_index = 0;
                                      ++key;
                                    }
                                  }
                                  pages[i]->SetUptodate();
                                }
                              }
                            });
        status != ZX_OK) {
      return zx::error(status);
    }
  }
  return zx::ok();
}

zx_status_t Reader::RunIO(StorageOperations &operation, OperationCallback callback) {
  zx_status_t ret = ZX_OK;
  if (!operation.IsEmpty()) {
    ret = bc_->RunRequests(operation.TakeOperations());
  }
  return operation.Completion(ret, std::move(callback));
}

}  // namespace f2fs

// namespace f2fs
