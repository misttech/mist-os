// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_MIGRATION_UTIL_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_MIGRATION_UTIL_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <zircon/pixelformat.h>

#include <cstdint>

#include <fbl/vector.h>

#include "src/graphics/display/lib/pixel-format/pixel-format.h"

namespace display {

// Pixel format that the display coordinator can use internally.
struct CoordinatorPixelFormat {
 public:
  // Converts a pixel format value used in banjo fuchsia.hardware.display
  // interface to a CoordinatorPixelFormat. The argument type must match
  // the `pixel_format` fields used in banjo fuchsia.hardware.display interface.
  static CoordinatorPixelFormat FromBanjo(zx_pixel_format_t banjo_pixel_format);

  // Creates a fbl::Vector containing converted pixel formats from a given
  // banjo-typed Vector got from display engine drivers. Returned values may get
  // de-duplicated.
  static zx::result<fbl::Vector<CoordinatorPixelFormat>> CreateFblVectorFromBanjoVector(
      cpp20::span<const zx_pixel_format_t> banjo_pixel_formats);

  // Converts a CoordinatorPixelFormat to format used in FIDL fuchsia.hardware.
  // display interface. The return type must match return the `pixel_format`
  // fields used in FIDL fuchsia.hardware.display interface.
  AnyPixelFormat ToFidl() const;

  // TODO(fxbug.dev/126114): During pixel_format migration, the underlying
  // format may be either `zx_pixel_format_t` or `fuchsia.images2.PixelFormat`.
  // The type of the underlying `format` reflects this.
  AnyPixelFormat format;
};

constexpr bool operator==(const CoordinatorPixelFormat& lhs, const CoordinatorPixelFormat& rhs) {
  return lhs.format == rhs.format;
}

constexpr bool operator!=(const CoordinatorPixelFormat& lhs, const CoordinatorPixelFormat& rhs) {
  return !(lhs == rhs);
}

// Cursor metadata that the display coordinator can use internally.
struct CoordinatorCursorInfo {
 public:
  // Converts a banjo fuchsia.hardware.display.CursorInfo typed struct to a
  // CoordinatorCursorInfo.
  static CoordinatorCursorInfo FromBanjo(const cursor_info_t& banjo_cursor_info);

  // Creates a fbl::Vector containing converted CursorInfos from a given
  // banjo-typed Vector got from display engine drivers. Returned values may get
  // de-duplicated.
  static zx::result<fbl::Vector<CoordinatorCursorInfo>> CreateFblVectorFromBanjoVector(
      cpp20::span<const cursor_info_t> banjo_cursor_infos);

  // Converts a CoordinatorCursorInfo to the FIDL fuchsia.hardware.
  // display.CursorInfo interface.
  fuchsia_hardware_display::wire::CursorInfo ToFidl() const;

  uint32_t width;
  uint32_t height;
  CoordinatorPixelFormat pixel_format;
};

constexpr bool operator==(const CoordinatorCursorInfo& lhs, const CoordinatorCursorInfo& rhs) {
  return lhs.width == rhs.width && lhs.height == rhs.height && lhs.pixel_format == rhs.pixel_format;
}

constexpr bool operator!=(const CoordinatorCursorInfo& lhs, const CoordinatorCursorInfo& rhs) {
  return !(lhs == rhs);
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_MIGRATION_UTIL_H_
