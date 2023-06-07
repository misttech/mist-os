// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_FRAME_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_FRAME_H_

#include <fidl/fuchsia.hardware.display/cpp/wire_types.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <cstdint>

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display/Frame`] and the banjo
// type [`fuchsia.hardware.display.controller/Frame`].
//
// See `::fuchsia_hardware_display::wire::Frame` for references.
//
// Note that the struct uses signed `int32_t` values for all coordinate and size
// fields instead of the unsigned `uint32` used by FIDL / banjo counterparts.
//
// All fields must be >= 0 and <= 2^31 - 1 for type conversion safety.
struct Frame {
  // Equivalent to the `x_pos` field of [`fuchsia.hardware.display/Frame`].
  // Must be >= 0 and <= 2^31 - 1.
  int32_t x_pos;

  // Equivalent to the `y_pos` field of [`fuchsia.hardware.display/Frame`].
  // Must be >= 0 and <= 2^31 - 1.
  int32_t y_pos;

  // Equivalent to the `width` field of [`fuchsia.hardware.display/Frame`].
  // Must be >= 0 and <= 2^31 - 1.
  int32_t width;

  // Equivalent to the `height` field of [`fuchsia.hardware.display/Frame`].
  // Must be >= 0 and <= 2^31 - 1.
  int32_t height;
};

inline bool operator==(const Frame& lhs, const Frame& rhs) {
  return lhs.x_pos == rhs.x_pos && lhs.y_pos == rhs.y_pos && lhs.width == rhs.width &&
         lhs.height == rhs.height;
}

inline bool operator!=(const Frame& lhs, const Frame& rhs) { return !(lhs == rhs); }

Frame ToFrame(const fuchsia_hardware_display::wire::Frame& frame_fidl);
Frame ToFrame(const frame_t& frame_banjo);

fuchsia_hardware_display::wire::Frame ToFidlFrame(const Frame& frame);
frame_t ToBanjoFrame(const Frame& frame);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_FRAME_H_
