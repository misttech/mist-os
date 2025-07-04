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

#include "src/storage/f2fs/layout.h"
#include "src/storage/f2fs/segment.h"
#include "src/storage/fvm/client.h"
#include "src/storage/lib/block_client/cpp/remote_block_device.h"

namespace f2fs {

zx::result<std::unique_ptr<BcacheMapper>> CreateBcacheMapper(
    std::vector<std::unique_ptr<block_client::BlockDevice>> devices, bool allocate) {
  uint64_t total_block_count = 0;
  constexpr uint32_t kMinVolumeSize = kMinVolumeSegments * kDefaultSegmentSize;

  std::vector<std::unique_ptr<Bcache>> bcaches;
  for (auto& device : devices) {
    fuchsia_hardware_block::wire::BlockInfo info;
    if (zx_status_t status = device->BlockGetInfo(&info); status != ZX_OK) {
      FX_LOGS(ERROR) << "Could not access device info: " << status;
      return zx::error(status);
    }

    if (info.block_size == 0 || kBlockSize % info.block_size != 0 ||
        info.block_size < kDefaultSectorSize || info.block_size > kBlockSize) {
      FX_LOGS(ERROR) << info.block_size << " of block size is not supported";
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    uint64_t block_count = info.block_size * info.block_count / kBlockSize;
    fuchsia_hardware_block_volume::wire::VolumeManagerInfo manager_info;
    fuchsia_hardware_block_volume::wire::VolumeInfo volume_info;
    bool use_fvm = device->VolumeGetInfo(&manager_info, &volume_info) == ZX_OK;
    if (use_fvm) {
      size_t slice_size = manager_info.slice_size;
      size_t slices_per_segment = kDefaultSegmentSize <= manager_info.slice_size
                                      ? 1
                                      : kDefaultSegmentSize / manager_info.slice_size;
      size_t slice_count = volume_info.partition_slice_count;
      ZX_ASSERT_MSG(kDefaultSegmentSize % manager_info.slice_size == 0 ||
                        manager_info.slice_size % kDefaultSegmentSize == 0,
                    " slice_size is not aligned with segment boundaries %lu",
                    manager_info.slice_size);

      if (allocate) {
        slice_count = fbl::round_down(slice_count, slices_per_segment);
        if (zx_status_t status = fvm::ResetAllSlices(device.get()); status != ZX_OK) {
          FX_LOGS(ERROR) << "failed to reset FVM slices: " << zx_status_get_string(status);
          return zx::error(status);
        }
        device->VolumeGetInfo(&manager_info, &volume_info);

        size_t free = manager_info.slice_count - manager_info.assigned_slice_count;
        size_t max_allowable =
            std::min(free + volume_info.partition_slice_count, manager_info.maximum_slice_count);
        if (volume_info.slice_limit) {
          max_allowable = std::min(volume_info.slice_limit, max_allowable);
        }
        max_allowable = fbl::round_down(max_allowable, slices_per_segment);

        if (slice_count > max_allowable || slice_count < kMinVolumeSize / slice_size) {
          FX_LOGS(WARNING) << "reset slice_count to " << max_allowable;
          slice_count = max_allowable;
        }

        size_t offset = volume_info.partition_slice_count;
        size_t length = slice_count - volume_info.partition_slice_count;

        if (zx_status_t status = device->VolumeExtend(offset, length); status != ZX_OK) {
          FX_LOGS(ERROR) << "failed to extend volume to (" << offset << ", " << length << ") "
                         << zx_status_get_string(status);
          return zx::error(status);
        }
      }
      FX_LOGS(INFO) << "slice_size: " << slice_size << ", slice_count: " << slice_count;
      block_count = slice_size * slice_count / kBlockSize;
    }

    auto bcache_or = Bcache::Create(std::move(device), block_count, kBlockSize);
    if (bcache_or.is_error()) {
      return bcache_or.take_error();
    }
    bcaches.push_back(std::move(bcache_or.value()));
    total_block_count += block_count;
  }
  if (total_block_count < kMinVolumeSize / kBlockSize) {
    FX_LOGS(ERROR) << "block device is too small";
    return zx::error(ZX_ERR_NO_SPACE);
  }
  // The maximum volume size of f2fs is 16TiB
  if (total_block_count >= std::numeric_limits<uint32_t>::max()) {
    FX_LOGS(ERROR) << "block device is too large (> 16TiB)";
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  return BcacheMapper::Create(std::move(bcaches));
}

zx::result<std::unique_ptr<BcacheMapper>> CreateBcacheMapper(
    std::unique_ptr<block_client::BlockDevice> device, bool allocate) {
  std::vector<std::unique_ptr<block_client::BlockDevice>> devices;
  devices.push_back(std::move(device));
  return CreateBcacheMapper(std::move(devices), allocate);
}

zx::result<std::unique_ptr<BcacheMapper>> CreateBcacheMapper(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device_channel, bool allocate) {
  auto device_or = block_client::RemoteBlockDevice::Create(
      fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>{device_channel.TakeChannel()});
  if (device_or.is_error()) {
    FX_LOGS(ERROR) << "could not initialize block device";
    return device_or.take_error();
  }

  auto bc_or = CreateBcacheMapper(std::move(*device_or), allocate);
  if (bc_or.is_error()) {
    FX_LOGS(ERROR) << "could not create block cache";
    return bc_or.take_error();
  }
  return bc_or.take_value();
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
  if (zx_status_t status = bcache->GetDevice()->BlockGetInfo(&bcache->info_); status != ZX_OK) {
    FX_LOGS(ERROR) << "cannot get block device information: " << status;
    return zx::error(status);
  }
  return zx::ok(std::move(bcache));
}

zx_status_t BcacheMapper::Readblk(block_t bno, void* data) {
  if (bno >= Maxblk()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::lock_guard lock(buffer_mutex_);
  zx_status_t status =
      RunRequests({storage::BufferedOperation{.vmoid = buffer_.vmoid(),
                                              .op = {.type = storage::OperationType::kRead,
                                                     .vmo_offset = 0,
                                                     .dev_offset = bno,
                                                     .length = 1}}});
  if (status != ZX_OK) {
    return status;
  }
  std::memcpy(data, buffer_.Data(0), BlockSize());
  return ZX_OK;
}

zx_status_t BcacheMapper::Writeblk(block_t bno, const void* data) {
  if (bno >= Maxblk()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::lock_guard lock(buffer_mutex_);
  std::memcpy(buffer_.Data(0), data, BlockSize());
  return RunRequests({storage::BufferedOperation{.vmoid = buffer_.vmoid(),
                                                 .op = {.type = storage::OperationType::kWrite,
                                                        .vmo_offset = 0,
                                                        .dev_offset = bno,
                                                        .length = 1}}});
}

zx_status_t BcacheMapper::Trim(block_t start, block_t num) {
  if (!(info_.flags & fuchsia_hardware_block::wire::Flag::kTrimSupport)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  return RunRequests({storage::BufferedOperation{.vmoid = BLOCK_VMOID_INVALID,
                                                 .op = {
                                                     .type = storage::OperationType::kTrim,
                                                     .vmo_offset = 0,
                                                     .dev_offset = start,
                                                     .length = num,
                                                 }}});
}

zx_status_t BcacheMapper::Flush() {
  for (auto& bcache : bcaches_) {
    if (auto err = bcache->Flush(); err != ZX_OK) {
      return err;
    }
  }
  return ZX_OK;
}

zx::result<std::unique_ptr<BcacheMapper>> BcacheMapper::Create(
    std::vector<std::unique_ptr<Bcache>> bcaches) {
  uint64_t total_block_count = 0;
  for (auto& bcache : bcaches) {
    total_block_count += bcache->Maxblk();
  }

  auto bcache = std::unique_ptr<BcacheMapper>(
      new BcacheMapper(std::move(bcaches), total_block_count, kBlockSize));
  return zx::ok(std::move(bcache));
}

zx_status_t BcacheMapper::RunRequests(const std::vector<storage::BufferedOperation>& operations) {
  std::vector<std::vector<storage::BufferedOperation>> new_operations(bcaches_.size());
  for (const auto& operation : operations) {
    if (operation.op.dev_offset + operation.op.length > Maxblk()) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    uint64_t residual = operation.op.length;
    uint64_t offset = operation.op.dev_offset;

    for (size_t i = 0; i < bcaches_.size(); ++i) {
      if (offset >= bcaches_[i]->Maxblk()) {
        offset -= bcaches_[i]->Maxblk();
        continue;
      }

      uint64_t length;
      if (residual <= bcaches_[i]->Maxblk() - offset) {
        length = residual;
      } else {
        length = bcaches_[i]->Maxblk() - offset;
      }

      vmoid_t vmoid;
      if (operation.vmoid == BLOCK_VMOID_INVALID) {
        if (operation.op.type != storage::OperationType::kTrim) {
          return ZX_ERR_INVALID_ARGS;
        }
        vmoid = BLOCK_VMOID_INVALID;
      } else {
        vmoid = vmoid_tree_[operation.vmoid][i].get();
      }

      new_operations[i].push_back(storage::BufferedOperation{
          .vmoid = vmoid,
          .op = storage::Operation{
              .type = operation.op.type,
              .vmo_offset = operation.op.vmo_offset + operation.op.length - residual,
              .dev_offset = offset,
              .length = length,
              .trace_flow_id = operation.op.trace_flow_id,
          }});

      residual -= length;
      offset = 0;
      if (residual == 0) {
        break;
      }
    }
  }

  for (size_t i = 0; i < bcaches_.size(); ++i) {
    if (new_operations[i].empty()) {
      continue;
    }

    if (auto err = bcaches_[i]->RunRequests(new_operations[i]); err != ZX_OK) {
      return err;
    }
  }
  return ZX_OK;
}

zx::result<vmoid_t> BcacheMapper::FindFreeVmoId() {
  for (vmoid_t i = last_id_; i < std::numeric_limits<vmoid_t>::max(); ++i) {
    if (vmoid_tree_.find(i) == vmoid_tree_.end()) {
      last_id_ = static_cast<vmoid_t>(i + 1);
      return zx::ok(i);
    }
  }
  for (vmoid_t i = BLOCK_VMOID_INVALID + 1; i < last_id_; ++i) {
    if (vmoid_tree_.find(i) == vmoid_tree_.end()) {
      last_id_ = static_cast<vmoid_t>(i + 1);
      return zx::ok(i);
    }
  }
  return zx::error(ZX_ERR_NO_RESOURCES);
}

zx_status_t BcacheMapper::BlockAttachVmo(const zx::vmo& vmo, storage::Vmoid* out) {
  zx::result<vmoid_t> vmoid_or = FindFreeVmoId();
  if (vmoid_or.is_error()) {
    return vmoid_or.error_value();
  }

  std::vector<storage::Vmoid> new_vmoids;
  auto cleanup = fit::defer([&] {
    for (size_t i = 0; i < new_vmoids.size(); ++i) {
      bcaches_[i]->BlockDetachVmo(std::move(new_vmoids[i]));
    }
  });

  for (auto& bcache : bcaches_) {
    storage::Vmoid vmoid;
    if (auto ret = bcache->BlockAttachVmo(vmo, &vmoid); ret != ZX_OK) {
      return ret;
    }
    new_vmoids.push_back(std::move(vmoid));
  }

  vmoid_tree_.insert(std::make_pair(vmoid_or.value(), std::move(new_vmoids)));
  *out = storage::Vmoid(vmoid_or.value());
  cleanup.cancel();
  return ZX_OK;
}

zx_status_t BcacheMapper::BlockDetachVmo(storage::Vmoid vmoid) {
  auto it = vmoid_tree_.find(vmoid.get());
  if (it == vmoid_tree_.end()) {
    return ZX_ERR_NOT_FOUND;
  }

  ZX_DEBUG_ASSERT(it->second.size() == bcaches_.size());
  auto& vmoids = it->second;
  for (size_t i = 0; i < bcaches_.size(); ++i) {
    if (auto ret = bcaches_[i]->BlockDetachVmo(std::move(vmoids[i])); ret != ZX_OK) {
      return ret;
    }
  }

  vmoid_tree_.erase(it);
  [[maybe_unused]] auto leak = vmoid.TakeId();
  return ZX_OK;
}

BcacheMapper::BcacheMapper(std::vector<std::unique_ptr<Bcache>> bcaches, uint64_t max_blocks,
                           block_t block_size)
    : bcaches_(std::move(bcaches)), block_size_(block_size), max_blocks_(max_blocks) {
  uint32_t transfer_size = MAX_TRANSFER_UNBOUNDED;
  uint32_t max_block_size = 0;
  fuchsia_hardware_block::wire::Flag flag = fuchsia_hardware_block::wire::Flag::kRemovable |
                                            fuchsia_hardware_block::wire::Flag::kTrimSupport |
                                            fuchsia_hardware_block::wire::Flag::kFuaSupport;
  for (auto& bcache : bcaches_) {
    fuchsia_hardware_block::wire::BlockInfo info;
    bcache->GetDevice()->BlockGetInfo(&info);

    if (max_block_size < info.block_size) {
      max_block_size = info.block_size;
    }

    if (transfer_size > info.max_transfer_size) {
      transfer_size = info.max_transfer_size;
    }

    if (info.flags & fuchsia_hardware_block::wire::Flag::kReadonly) {
      flag |= fuchsia_hardware_block::wire::Flag::kReadonly;
      read_only_ = true;
    }
    if (!(info.flags & fuchsia_hardware_block::wire::Flag::kRemovable)) {
      flag &= (~fuchsia_hardware_block::wire::Flag::kRemovable);
    }
    if (!(info.flags & fuchsia_hardware_block::wire::Flag::kTrimSupport)) {
      flag &= (~fuchsia_hardware_block::wire::Flag::kTrimSupport);
    }
    if (!(info.flags & fuchsia_hardware_block::wire::Flag::kFuaSupport)) {
      flag &= (~fuchsia_hardware_block::wire::Flag::kFuaSupport);
    }
  }
  uint64_t device_block_count = max_blocks * block_size / max_block_size;

  info_ = {
      .block_count = device_block_count,
      .block_size = max_block_size,
      .max_transfer_size = transfer_size,
      .flags = flag,
  };
  buffer_.Initialize(this, 1, info_.block_size, "scratch-block");
}

zx_status_t BcacheMapper::BlockGetInfo(fuchsia_hardware_block::wire::BlockInfo* out_info) const {
  *out_info = info_;
  return ZX_OK;
}

}  // namespace f2fs
