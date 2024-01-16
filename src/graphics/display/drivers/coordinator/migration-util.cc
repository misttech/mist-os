// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/migration-util.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>

namespace display {

// static
CoordinatorPixelFormat CoordinatorPixelFormat::FromBanjo(
    fuchsia_images2_pixel_format_enum_value_t banjo_pixel_format) {
  return {.format = static_cast<fuchsia_images2::wire::PixelFormat>(banjo_pixel_format)};
}

// static
zx::result<fbl::Vector<CoordinatorPixelFormat>>
CoordinatorPixelFormat::CreateFblVectorFromBanjoVector(
    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats) {
  fbl::AllocChecker alloc_checker;
  fbl::Vector<CoordinatorPixelFormat> result;
  result.reserve(banjo_pixel_formats.size(), &alloc_checker);
  if (!alloc_checker.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  for (const fuchsia_images2_pixel_format_enum_value_t banjo_pixel_format : banjo_pixel_formats) {
    result.push_back(CoordinatorPixelFormat::FromBanjo(banjo_pixel_format));
  }
  return zx::ok(std::move(result));
}

fuchsia_images2::wire::PixelFormat CoordinatorPixelFormat::ToFidl() const { return format; }

// static
CoordinatorCursorInfo CoordinatorCursorInfo::FromBanjo(const cursor_info_t& banjo_cursor_info) {
  return CoordinatorCursorInfo{
      .width = banjo_cursor_info.width,
      .height = banjo_cursor_info.height,
      .pixel_format = CoordinatorPixelFormat::FromBanjo(banjo_cursor_info.format),
  };
}

// static
zx::result<fbl::Vector<CoordinatorCursorInfo>>
CoordinatorCursorInfo::CreateFblVectorFromBanjoVector(
    cpp20::span<const cursor_info_t> banjo_cursor_infos) {
  fbl::AllocChecker alloc_checker;
  fbl::Vector<CoordinatorCursorInfo> result;
  result.reserve(banjo_cursor_infos.size(), &alloc_checker);
  if (!alloc_checker.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  for (const cursor_info_t& banjo_cursor_info : banjo_cursor_infos) {
    result.push_back(CoordinatorCursorInfo::FromBanjo(banjo_cursor_info));
  }
  return zx::ok(std::move(result));
}

fuchsia_hardware_display_types::wire::CursorInfo CoordinatorCursorInfo::ToFidl() const {
  return {
      .width = width,
      .height = height,
      .pixel_format = pixel_format.ToFidl(),
  };
}

}  // namespace display
