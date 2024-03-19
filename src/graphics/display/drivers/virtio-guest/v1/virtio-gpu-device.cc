// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/virtio-gpu-device.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cinttypes>
#include <cstdint>
#include <memory>
#include <optional>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/virtio-guest/v1/virtio-pci-device.h"
#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

VirtioGpuDevice::VirtioGpuDevice(std::unique_ptr<VirtioPciDevice> virtio_device)
    : virtio_device_(std::move(virtio_device)) {
  ZX_DEBUG_ASSERT(virtio_device_);
}

VirtioGpuDevice::~VirtioGpuDevice() = default;

zx::result<uint32_t> VirtioGpuDevice::UpdateCursor() {
  const virtio_abi::UpdateCursorCommand command = {
      .header = {.type = virtio_abi::ControlType::kUpdateCursorCommand},
      .resource_id = next_resource_id_++,
  };

  const auto& response =
      virtio_device_->ExchangeCursorqRequestResponse<virtio_abi::EmptyResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(WARNING, "Unexpected response type: %s (0x%04" PRIx32 ")",
           ControlTypeToString(response.header.type), static_cast<uint32_t>(response.header.type));
    return zx::error(ZX_ERR_IO);
  }

  return zx::ok(command.resource_id);
}  // namespace virtio_display

zx::result<fbl::Vector<DisplayInfo>> VirtioGpuDevice::GetDisplayInfo() {
  const virtio_abi::GetDisplayInfoCommand command = {
      .header = {.type = virtio_abi::ControlType::kGetDisplayInfoCommand},
  };

  const auto& response =
      virtio_device_->ExchangeControlqRequestResponse<virtio_abi::DisplayInfoResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kDisplayInfoResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04" PRIx32 ")",
           ControlTypeToString(response.header.type), static_cast<uint32_t>(response.header.type));
    return zx::error(ZX_ERR_IO);
  }

  size_t enabled_scanout_count = 0;
  for (const virtio_abi::ScanoutInfo& scanout : response.scanouts) {
    if (scanout.enabled) {
      ++enabled_scanout_count;
    }
  }

  fbl::AllocChecker alloc_checker;
  fbl::Vector<DisplayInfo> display_infos;
  display_infos.reserve(enabled_scanout_count, &alloc_checker);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayInfo vector");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  for (int i = 0; i < virtio_abi::kMaxScanouts; ++i) {
    const virtio_abi::ScanoutInfo& scanout = response.scanouts[i];
    if (!scanout.enabled) {
      continue;
    }

    zxlogf(TRACE,
           "Scanout %d: placement (%" PRIu32 ", %" PRIu32 "), resolution %" PRIu32 "x%" PRIu32
           " flags 0x%08" PRIx32,
           i, scanout.geometry.placement_x, scanout.geometry.placement_y, scanout.geometry.width,
           scanout.geometry.height, scanout.flags);

    ZX_DEBUG_ASSERT(display_infos.size() < enabled_scanout_count);
    display_infos.push_back({.scanout_info = scanout, .scanout_id = i}, &alloc_checker);
    ZX_DEBUG_ASSERT(alloc_checker.check());
  }
  return zx::ok(std::move(display_infos));
}

namespace {

// Returns nullopt for an unsupported format.
std::optional<virtio_abi::ResourceFormat> To2DResourceFormat(
    fuchsia_images2::wire::PixelFormat pixel_format) {
  // TODO(https://fxbug.dev/42073721): Support more formats.
  switch (pixel_format) {
    case fuchsia_images2::PixelFormat::kB8G8R8A8:
      return virtio_abi::ResourceFormat::kBgra32;
    default:
      return std::nullopt;
  }
}

}  // namespace

zx::result<uint32_t> VirtioGpuDevice::Create2DResource(
    uint32_t width, uint32_t height, fuchsia_images2::wire::PixelFormat pixel_format) {
  zxlogf(TRACE, "Allocate2DResource");

  std::optional<virtio_abi::ResourceFormat> resource_format = To2DResourceFormat(pixel_format);
  if (!resource_format.has_value()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  const virtio_abi::Create2DResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kCreate2DResourceCommand},
      .resource_id = next_resource_id_++,
      .format = virtio_abi::ResourceFormat::kBgra32,
      .width = width,
      .height = height,
  };

  const auto& response =
      virtio_device_->ExchangeControlqRequestResponse<virtio_abi::EmptyResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04" PRIx32 ")",
           ControlTypeToString(response.header.type), static_cast<uint32_t>(response.header.type));
    return zx::error(ZX_ERR_IO);
  }
  return zx::ok(command.resource_id);
}

zx::result<> VirtioGpuDevice::AttachResourceBacking(uint32_t resource_id, zx_paddr_t ptr,
                                                    size_t buf_len) {
  ZX_ASSERT(ptr);

  zxlogf(TRACE,
         "AttachResourceBacking - resource ID %" PRIu32 ", address 0x%" PRIx64 ", length %zu",
         resource_id, ptr, buf_len);

  const virtio_abi::AttachResourceBackingCommand<1> command = {
      .header = {.type = virtio_abi::ControlType::kAttachResourceBackingCommand},
      .resource_id = resource_id,
      .entries =
          {
              {.address = ptr, .length = static_cast<uint32_t>(buf_len)},
          },
  };

  const auto& response =
      virtio_device_->ExchangeControlqRequestResponse<virtio_abi::EmptyResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04" PRIx32 ")",
           ControlTypeToString(response.header.type), static_cast<uint32_t>(response.header.type));
    return zx::error(ZX_ERR_IO);
  }
  return zx::ok();
}

zx::result<> VirtioGpuDevice::SetScanoutProperties(uint32_t scanout_id, uint32_t resource_id,
                                                   uint32_t width, uint32_t height) {
  zxlogf(TRACE,
         "SetScanoutProperties - scanout ID %" PRIu32 ", resource ID %" PRIu32 ", size %" PRIu32
         "x%" PRIu32,
         scanout_id, resource_id, width, height);

  const virtio_abi::SetScanoutCommand command = {
      .header = {.type = virtio_abi::ControlType::kSetScanoutCommand},
      .geometry =
          {
              .placement_x = 0,
              .placement_y = 0,
              .width = width,
              .height = height,
          },
      .scanout_id = scanout_id,
      .resource_id = resource_id,
  };

  const auto& response =
      virtio_device_->ExchangeControlqRequestResponse<virtio_abi::EmptyResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04" PRIx32 ")",
           ControlTypeToString(response.header.type), static_cast<uint32_t>(response.header.type));
    return zx::error(ZX_ERR_IO);
  }
  return zx::ok();
}

zx::result<> VirtioGpuDevice::FlushResource(uint32_t resource_id, uint32_t width, uint32_t height) {
  zxlogf(TRACE, "FlushResource - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32, resource_id,
         width, height);

  virtio_abi::FlushResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kFlushResourceCommand},
      .geometry = {.placement_x = 0, .placement_y = 0, .width = width, .height = height},
      .resource_id = resource_id,
  };

  const auto& response =
      virtio_device_->ExchangeControlqRequestResponse<virtio_abi::EmptyResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04" PRIx32 ")",
           ControlTypeToString(response.header.type), static_cast<uint32_t>(response.header.type));
    return zx::error(ZX_ERR_IO);
  }
  return zx::ok();
}

zx::result<> VirtioGpuDevice::TransferToHost2D(uint32_t resource_id, uint32_t width,
                                               uint32_t height) {
  zxlogf(TRACE, "Transfer2DResourceToHost - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32,
         resource_id, width, height);

  virtio_abi::Transfer2DResourceToHostCommand command = {
      .header = {.type = virtio_abi::ControlType::kTransfer2DResourceToHostCommand},
      .geometry =
          {
              .placement_x = 0,
              .placement_y = 0,
              .width = width,
              .height = height,
          },
      .destination_offset = 0,
      .resource_id = resource_id,
  };

  const auto& response =
      virtio_device_->ExchangeControlqRequestResponse<virtio_abi::EmptyResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04" PRIx32 ")",
           ControlTypeToString(response.header.type), static_cast<uint32_t>(response.header.type));
    return zx::error(ZX_ERR_IO);
  }
  return zx::ok();
}

}  // namespace virtio_display
