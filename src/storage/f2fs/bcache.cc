// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/bcache.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <storage/buffer/block_buffer.h>
#include <storage/buffer/vmo_buffer.h>
#include <storage/operation/operation.h>

#include "src/lib/storage/block_client/cpp/remote_block_device.h"
#include "src/storage/f2fs/f2fs_layout.h"
#include "src/storage/f2fs/f2fs_lib.h"

namespace f2fs {

zx::result<std::unique_ptr<Bcache>> CreateBcache(std::unique_ptr<block_client::BlockDevice> device,
                                                 bool* out_readonly) {
  fuchsia_hardware_block::wire::BlockInfo info;
  if (zx_status_t status = device->BlockGetInfo(&info); status != ZX_OK) {
    FX_LOGS(ERROR) << "Coult not access device info: " << status;
    return zx::error(status);
  }

  uint64_t device_size = info.block_size * info.block_count;
  // f2fs requires at least 8 segments (sb + (ckpt + sit + nat) * 2 + ssr) for its metadata.
  constexpr uint64_t min_size = kBlockSize * kDefaultBlocksPerSegment * 8;

  if (device_size <= min_size) {
    FX_LOGS(ERROR) << "block device is too small";
    return zx::error(ZX_ERR_NO_SPACE);
  }
  uint64_t block_count = device_size / kBlockSize;

  // The maximum volume size of f2fs is 16TiB
  if (block_count >= std::numeric_limits<uint32_t>::max()) {
    FX_LOGS(ERROR) << "block device is too large (> 16TiB)";
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  if (out_readonly) {
    *out_readonly = static_cast<bool>(info.flags & fuchsia_hardware_block::wire::Flag::kReadonly);
  }

  return Bcache::Create(std::move(device), block_count, kBlockSize);
}

zx::result<std::unique_ptr<Bcache>> CreateBcache(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device_channel, bool* out_readonly) {
  auto device_or = block_client::RemoteBlockDevice::Create(
      fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>{device_channel.TakeChannel()});
  if (device_or.is_error()) {
    FX_LOGS(ERROR) << "could not initialize block device";
    return device_or.take_error();
  }

  bool readonly_device = false;
  auto bc_or = CreateBcache(std::move(*device_or), &readonly_device);
  if (bc_or.is_error()) {
    FX_LOGS(ERROR) << "could not create block cache";
    return bc_or.take_error();
  }
  return bc_or.take_value();
}

std::unique_ptr<block_client::BlockDevice> Bcache::Destroy(std::unique_ptr<Bcache> bcache) {
  // Destroy the VmoBuffer before extracting the underlying device, as it needs
  // to de-register itself from the underlying block device to be terminated.
  bcache->DestroyVmoBuffer();
  return std::move(bcache->device_);
}

Bcache::Bcache(std::unique_ptr<block_client::BlockDevice> device, uint64_t max_blocks,
               block_t block_size)
    : max_blocks_(max_blocks), block_size_(block_size), device_(std::move(device)) {}

zx_status_t Bcache::BlockAttachVmo(const zx::vmo& vmo, storage::Vmoid* out) {
  return GetDevice()->BlockAttachVmo(vmo, out);
}

zx_status_t Bcache::BlockDetachVmo(storage::Vmoid vmoid) {
  return GetDevice()->BlockDetachVmo(std::move(vmoid));
}

zx::result<std::unique_ptr<Bcache>> Bcache::Create(
    std::unique_ptr<block_client::BlockDevice> device, uint64_t max_blocks, block_t block_size) {
  std::unique_ptr<Bcache> bcache(new Bcache(std::move(device), max_blocks, block_size));

  zx_status_t status = bcache->CreateVmoBuffer();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  status = bcache->VerifyDeviceInfo();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(bcache));
}

zx_status_t Bcache::Readblk(block_t bno, void* data) {
  if (bno >= max_blocks_) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  storage::Operation operation = {};
  operation.type = storage::OperationType::kRead;
  operation.vmo_offset = 0;
  operation.dev_offset = bno;
  operation.length = 1;
  std::lock_guard lock(buffer_mutex_);
  zx_status_t status = RunOperation(operation, &buffer_);
  if (status != ZX_OK) {
    return status;
  }
  std::memcpy(data, buffer_.Data(0), BlockSize());
  return ZX_OK;
}

zx_status_t Bcache::Writeblk(block_t bno, const void* data) {
  if (bno >= max_blocks_) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  storage::Operation operation = {};
  operation.type = storage::OperationType::kWrite;
  operation.vmo_offset = 0;
  operation.dev_offset = bno;
  operation.length = 1;
  std::lock_guard lock(buffer_mutex_);
  std::memcpy(buffer_.Data(0), data, BlockSize());
  return RunOperation(operation, &buffer_);
}

zx_status_t Bcache::RunRequests(const std::vector<storage::BufferedOperation>& operations) {
  std::shared_lock lock(mutex_);
  return DeviceTransactionHandler::RunRequests(operations);
}

zx_status_t Bcache::VerifyDeviceInfo() {
  zx_status_t status = device_->BlockGetInfo(&info_);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "cannot get block device information: " << status;
    return status;
  }

  if (BlockSize() % info_.block_size != 0) {
    FX_LOGS(WARNING) << "f2fs block size cannot be multiple of underlying block size: "
                     << info_.block_size;
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

zx_status_t Bcache::Trim(block_t start, block_t num) {
  if (!(info_.flags & fuchsia_hardware_block::wire::Flag::kTrimSupport)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  block_fifo_request_t request = {
      .command = {.opcode = BLOCK_OPCODE_TRIM, .flags = 0},
      .vmoid = BLOCK_VMOID_INVALID,
      .length = static_cast<uint32_t>(BlockNumberToDevice(num)),
      .vmo_offset = 0,
      .dev_offset = BlockNumberToDevice(start),
  };

  return GetDevice()->FifoTransaction(&request, 1);
}

}  // namespace f2fs
