// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_MIGRATION_UTIL_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_MIGRATION_UTIL_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/zx/result.h>

#include <span>

#include <fbl/vector.h>

namespace display_coordinator {

// Pixel format that the display coordinator can use internally.
struct CoordinatorPixelFormat {
 public:
  // Converts a pixel format value used in banjo fuchsia.hardware.display
  // interface to a CoordinatorPixelFormat. The argument type must match
  // the `pixel_format` fields used in banjo fuchsia.hardware.display interface.
  static CoordinatorPixelFormat FromBanjo(
      fuchsia_images2_pixel_format_enum_value_t banjo_pixel_format);

  // Creates a fbl::Vector containing converted pixel formats from a given
  // banjo-typed Vector got from display engine drivers. Returned values may get
  // de-duplicated.
  static zx::result<fbl::Vector<CoordinatorPixelFormat>> CreateFblVectorFromBanjoVector(
      std::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats);

  // Converts a CoordinatorPixelFormat to format used in FIDL fuchsia.hardware.
  // display interface. The return type must match return the `pixel_format`
  // fields used in FIDL fuchsia.hardware.display interface.
  fuchsia_images2::wire::PixelFormat ToFidl() const;

  fuchsia_images2::wire::PixelFormat format;
};

constexpr bool operator==(const CoordinatorPixelFormat& lhs, const CoordinatorPixelFormat& rhs) {
  return lhs.format == rhs.format;
}

constexpr bool operator!=(const CoordinatorPixelFormat& lhs, const CoordinatorPixelFormat& rhs) {
  return !(lhs == rhs);
}

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_MIGRATION_UTIL_H_
