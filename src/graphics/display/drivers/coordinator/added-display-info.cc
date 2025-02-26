// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/added-display-info.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>

#include <algorithm>
#include <cinttypes>
#include <cstdint>
#include <span>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display_coordinator {

// static
zx::result<std::unique_ptr<AddedDisplayInfo>> AddedDisplayInfo::Create(
    const raw_display_info_t& banjo_display_info) {
  const display::DisplayId display_id(banjo_display_info.display_id);
  if (display_id == display::kInvalidDisplayId) {
    FDF_LOG(ERROR, "AddedDisplayInfo creation failed: invalid display ID");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fbl::AllocChecker alloc_checker;
  const std::span<const uint8_t> display_info_edid_bytes(banjo_display_info.edid_bytes_list,
                                                         banjo_display_info.edid_bytes_count);
  fbl::Vector<uint8_t> edid_bytes;
  if (banjo_display_info.edid_bytes_count != 0) {
    edid_bytes.resize(banjo_display_info.edid_bytes_count, &alloc_checker);
    if (!alloc_checker.check()) {
      FDF_LOG(ERROR, "AddedDisplayInfo creation failed: out of memory allocating EDID bytes");
      return zx::error(ZX_ERR_NO_MEMORY);
    }
    std::ranges::copy(display_info_edid_bytes, edid_bytes.begin());
  }

  if (banjo_display_info.pixel_formats_count == 0) {
    FDF_LOG(ERROR, "AddedDisplayInfo creation failed: empty pixel formats list");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fbl::Vector<display::PixelFormat> pixel_formats;
  pixel_formats.reserve(banjo_display_info.pixel_formats_count, &alloc_checker);
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "AddedDisplayInfo creation failed: out of memory allocating pixel formats");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  for (size_t i = 0; i < banjo_display_info.pixel_formats_count; ++i) {
    const uint32_t banjo_pixel_format = banjo_display_info.pixel_formats_list[i];
    if (!display::PixelFormat::IsSupported(banjo_pixel_format)) {
      FDF_LOG(ERROR, "AddedDisplayInfo creation failed: unsupported pixel format %" PRIu32,
              banjo_pixel_format);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    display::PixelFormat pixel_format(banjo_pixel_format);

    ZX_DEBUG_ASSERT(pixel_formats.size() < banjo_display_info.pixel_formats_count);
    pixel_formats.push_back(pixel_format, &alloc_checker);
    ZX_DEBUG_ASSERT(alloc_checker.check());
  }

  fbl::Vector<display_mode_t> banjo_display_modes;
  if (banjo_display_info.preferred_modes_count != 0) {
    banjo_display_modes.reserve(banjo_display_info.preferred_modes_count, &alloc_checker);
    if (!alloc_checker.check()) {
      FDF_LOG(ERROR, "AddedDisplayInfo creation failed: out of memory allocating display modes");
      return zx::error(ZX_ERR_NO_MEMORY);
    }
    for (size_t i = 0; i < banjo_display_info.preferred_modes_count; ++i) {
      ZX_DEBUG_ASSERT(banjo_display_modes.size() < banjo_display_info.preferred_modes_count);
      banjo_display_modes.push_back(banjo_display_info.preferred_modes_list[i], &alloc_checker);
      ZX_DEBUG_ASSERT(alloc_checker.check());
    }
  }

  auto display_info = fbl::make_unique_checked<AddedDisplayInfo>(&alloc_checker);
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "AddedDisplayInfo creation failed: out of memory allocating instance");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  *display_info = {
      .display_id = display_id,
      .edid_bytes = std::move(edid_bytes),
      .pixel_formats = std::move(pixel_formats),
      .banjo_preferred_modes = std::move(banjo_display_modes),
  };
  return zx::ok(std::move(display_info));
}

}  // namespace display_coordinator
