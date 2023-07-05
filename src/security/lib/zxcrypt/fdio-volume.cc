// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/security/lib/zxcrypt/fdio-volume.h"

#include <errno.h>
#include <fidl/fuchsia.hardware.block.encrypted/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <inttypes.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fdio.h>
#include <lib/zircon-internal/debug.h>
#include <lib/zx/channel.h>
#include <unistd.h>
#include <zircon/status.h>

#include <memory>
#include <utility>

#include <fbl/string_buffer.h>
#include <fbl/vector.h>

#include "src/lib/storage/block_client/cpp/remote_block_device.h"
#include "src/security/lib/zxcrypt/volume.h"

#define ZXDEBUG 0

namespace zxcrypt {

FdioVolume::FdioVolume(fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel)
    : device_(std::move(channel)) {}

zx::result<std::unique_ptr<FdioVolume>> FdioVolume::Init(
    fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel) {
  if (!channel) {
    xprintf("bad parameter(s): block=%d\n", channel.channel().get());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fbl::AllocChecker ac;
  std::unique_ptr<FdioVolume> volume(new (&ac) FdioVolume(std::move(channel)));
  if (!ac.check()) {
    xprintf("allocation failed: %zu bytes\n", sizeof(FdioVolume));
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  if (zx_status_t status = volume->Init(); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(volume));
}

zx::result<std::unique_ptr<FdioVolume>> FdioVolume::Create(
    fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel, const crypto::Secret& key) {
  zx::result volume = Init(std::move(channel));
  if (volume.is_error()) {
    xprintf("Init failed: %s\n", volume.status_string());
    return volume.take_error();
  }

  uint8_t slot = 0;
  if (zx_status_t status = volume.value()->Format(key, slot); status != ZX_OK) {
    xprintf("Format failed: %s\n", zx_status_get_string(status));
    return zx::error(status);
  }
  return volume;
}

zx::result<std::unique_ptr<FdioVolume>> FdioVolume::Unlock(
    fidl::ClientEnd<fuchsia_hardware_block_volume::Volume> channel, const crypto::Secret& key,
    key_slot_t slot) {
  zx::result volume = Init(std::move(channel));
  if (volume.is_error()) {
    xprintf("Init failed: %s\n", volume.status_string());
    return volume.take_error();
  }
  if (zx_status_t status = volume.value()->Unlock(key, slot); status != ZX_OK) {
    xprintf("Unlock failed: %s\n", zx_status_get_string(status));
    return zx::error(status);
  }
  return volume;
}

zx_status_t FdioVolume::Unlock(const crypto::Secret& key, key_slot_t slot) {
  return Volume::Unlock(key, slot);
}

// Configuration methods
zx_status_t FdioVolume::Enroll(const crypto::Secret& key, key_slot_t slot) {
  if (zx_status_t status = SealBlock(key, slot); status != ZX_OK) {
    xprintf("SealBlock failed: %s\n", zx_status_get_string(status));
    return status;
  }
  if (zx_status_t status = CommitBlock(); status != ZX_OK) {
    xprintf("CommitBlock failed: %s\n", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t FdioVolume::Revoke(key_slot_t slot) {
  zx_off_t off;
  crypto::Bytes invalid;
  if (zx_status_t status = GetSlotOffset(slot, &off); status != ZX_OK) {
    xprintf("GetSlotOffset failed: %s\n", zx_status_get_string(status));
    return status;
  }
  if (zx_status_t status = invalid.Randomize(slot_len_); status != ZX_OK) {
    xprintf("Randomize failed: %s\n", zx_status_get_string(status));
    return status;
  }
  if (zx_status_t status = block_.Copy(invalid, off); status != ZX_OK) {
    xprintf("Copy failed: %s\n", zx_status_get_string(status));
    return status;
  }
  if (zx_status_t status = CommitBlock(); status != ZX_OK) {
    xprintf("CommitBlock failed: %s\n", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t FdioVolume::Init() { return Volume::Init(); }

zx_status_t FdioVolume::GetBlockInfo(BlockInfo* out) {
  const fidl::WireResult result = fidl::WireCall(device_)->GetInfo();
  if (!result.ok()) {
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }

  out->block_count = response.value()->info.block_count;
  out->block_size = response.value()->info.block_size;
  return ZX_OK;
}

zx_status_t FdioVolume::GetFvmSliceSize(uint64_t* out) {
  const fidl::WireResult result = fidl::WireCall(device_)->GetVolumeInfo();
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    return response.status;
  }

  *out = response.manager->slice_size;
  return ZX_OK;
}

zx_status_t FdioVolume::DoBlockFvmVsliceQuery(uint64_t vslice_start,
                                              SliceRegion ranges[Volume::MAX_SLICE_REGIONS],
                                              uint64_t* slice_count) {
  static_assert(fuchsia_hardware_block_volume::wire::kMaxSliceRequests == Volume::MAX_SLICE_REGIONS,
                "block volume slice response count must match");

  const fidl::WireResult result = fidl::WireCall(device_)->QuerySlices(
      fidl::VectorView<uint64_t>::FromExternal(&vslice_start, 1));
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    return response.status;
  }

  if (response.response_count > Volume::MAX_SLICE_REGIONS) {
    // Should be impossible.  Trust nothing.
    return ZX_ERR_BAD_STATE;
  }

  *slice_count = response.response_count;
  for (size_t i = 0; i < response.response_count; i++) {
    ranges[i].allocated = response.response[i].allocated;
    ranges[i].count = response.response[i].count;
  }

  return ZX_OK;
}

zx_status_t FdioVolume::DoBlockFvmExtend(uint64_t start_slice, uint64_t slice_count) {
  const fidl::WireResult result = fidl::WireCall(device_)->Extend(start_slice, slice_count);
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    return response.status;
  }

  return ZX_OK;
}

zx_status_t FdioVolume::Read() {
  // TODO(https://fxbug.dev/129956): Update this API to take a volume channel instead.
  return block_client::SingleReadBytes(
      fidl::UnownedClientEnd<fuchsia_hardware_block::Block>(device_.channel().borrow()),
      block_.get(), block_.len(), offset_);
}

zx_status_t FdioVolume::Write() {
  // TODO(https://fxbug.dev/129956): Update this API to take a volume channel instead.
  return block_client::SingleWriteBytes(
      fidl::UnownedClientEnd<fuchsia_hardware_block::Block>(device_.channel().borrow()),
      block_.get(), block_.len(), offset_);
}

zx_status_t FdioVolume::Flush() {
  // On Fuchsia, an FD produced by opening a block device out of the device tree doesn't implement
  // fsync(), so we stub this out.  FdioVolume is only used for tests anyway, which don't need to
  // worry too much about durability.
  return ZX_OK;
}

}  // namespace zxcrypt
