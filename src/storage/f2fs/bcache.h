// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file describes the in-memory structures which construct
// a F2FS filesystem.

#ifndef SRC_STORAGE_F2FS_BCACHE_H_
#define SRC_STORAGE_F2FS_BCACHE_H_

#include <lib/zx/vmo.h>
#include <zircon/compiler.h>

#include <fbl/algorithm.h>
#include <fbl/array.h>
#include <fbl/macros.h>
#include <fbl/unique_fd.h>
#include <storage/buffer/vmo_buffer.h>
#include <storage/buffer/vmoid_registry.h>

#include "src/lib/storage/block_client/cpp/block_device.h"
#include "src/lib/storage/vfs/cpp/transaction/device_transaction_handler.h"
#include "src/storage/f2fs/f2fs_types.h"

namespace f2fs {

class Bcache;
zx::result<std::unique_ptr<Bcache>> CreateBcache(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device, bool* out_readonly = nullptr);
zx::result<std::unique_ptr<Bcache>> CreateBcache(std::unique_ptr<block_client::BlockDevice> device,
                                                 bool* out_readonly = nullptr);
class Bcache : public fs::DeviceTransactionHandler, public storage::VmoidRegistry {
 public:
  // Not copyable or movable
  Bcache(const Bcache&) = delete;
  Bcache& operator=(const Bcache&) = delete;
  Bcache(Bcache&&) = delete;
  Bcache& operator=(Bcache&&) = delete;

  // Make a read/write operation from/to a 4KiB block at |bno|.
  // |buffer_| is used as a block buffer.
  zx_status_t Readblk(block_t bno, void* data) __TA_EXCLUDES(buffer_mutex_);
  zx_status_t Writeblk(block_t bno, const void* data) __TA_EXCLUDES(buffer_mutex_);
  zx_status_t Trim(block_t start, block_t num);

  // Blocks all I/O operations to the underlying device (that go via the RunRequests method). This
  // does *not* block operations that go directly to the device.
  // TODO(fxbug.dev/130250): change this to __TA_ACQUIRE(mutex_) after clang roll.
  void Pause() __TA_NO_THREAD_SAFETY_ANALYSIS { mutex_.lock(); }
  // Resumes all I/O operations paused by the Pause method.
  // TODO(fxbug.dev/130250): change this to __TA_RELEASE(mutex_) after clang roll.
  void Resume() __TA_NO_THREAD_SAFETY_ANALYSIS { mutex_.unlock(); }

  uint64_t Maxblk() const { return max_blocks_; }
  block_t BlockSize() const { return block_size_; }

  zx_status_t RunRequests(const std::vector<storage::BufferedOperation>& operations) override;
  // This factory allows building this object from a BlockDevice. Bcache can take ownership of the
  // device (the first Create method), or not (the second Create method).
  static zx::result<std::unique_ptr<Bcache>> Create(
      std::unique_ptr<block_client::BlockDevice> device, uint64_t max_blocks, block_t block_size);

  zx_status_t Flush() override { return DeviceTransactionHandler::Flush(); }

  zx_status_t BlockAttachVmo(const zx::vmo& vmo, storage::Vmoid* out) final;
  zx_status_t BlockDetachVmo(storage::Vmoid vmoid) final;

  zx_status_t CreateVmoBuffer() __TA_EXCLUDES(buffer_mutex_) {
    std::lock_guard lock(buffer_mutex_);
    return buffer_.Initialize(this, 1, block_size_, "scratch-block");
  }

  void DestroyVmoBuffer() __TA_EXCLUDES(buffer_mutex_) {
    std::lock_guard lock(buffer_mutex_);
    [[maybe_unused]] auto unused = std::move(buffer_);
  }

  uint64_t BlockNumberToDevice(uint64_t block_num) const final {
    return block_num * BlockSize() / info_.block_size;
  }

  uint64_t DeviceBlockSize() const { return info_.block_size; }

  block_client::BlockDevice* GetDevice() final { return device_.get(); }

  // Destroys a "bcache" object, but take back ownership of the underlying block device.
  static std::unique_ptr<block_client::BlockDevice> Destroy(std::unique_ptr<Bcache> bcache);

 private:
  friend class BlockNode;

  // Used during initialization of this object.
  zx_status_t VerifyDeviceInfo();

  const uint64_t max_blocks_;
  std::mutex buffer_mutex_;
  std::shared_mutex mutex_;

  Bcache(std::unique_ptr<block_client::BlockDevice> device, uint64_t max_blocks,
         block_t block_size);

  const block_t block_size_;
  fuchsia_hardware_block::wire::BlockInfo info_ = {};
  std::unique_ptr<block_client::BlockDevice> device_;  // The device, if owned.
  // |buffer_| and |buffer_mutex_| are used in the "Readblk/Writeblk" methods.
  storage::VmoBuffer buffer_ __TA_GUARDED(buffer_mutex_);
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_BCACHE_H_
