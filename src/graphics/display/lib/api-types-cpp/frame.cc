// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/frame.h"

#include <zircon/assert.h>

#include <limits>

namespace display {

Frame ToFrame(const fuchsia_hardware_display::wire::Frame& frame_fidl) {
  ZX_DEBUG_ASSERT(frame_fidl.x_pos <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(frame_fidl.y_pos <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(frame_fidl.width <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(frame_fidl.height <= std::numeric_limits<int32_t>::max());
  return Frame{
      .x_pos = static_cast<int32_t>(frame_fidl.x_pos),
      .y_pos = static_cast<int32_t>(frame_fidl.y_pos),
      .width = static_cast<int32_t>(frame_fidl.width),
      .height = static_cast<int32_t>(frame_fidl.height),
  };
}

Frame ToFrame(const frame_t& frame_banjo) {
  ZX_DEBUG_ASSERT(frame_banjo.x_pos <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(frame_banjo.y_pos <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(frame_banjo.width <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(frame_banjo.height <= std::numeric_limits<int32_t>::max());
  return Frame{
      .x_pos = static_cast<int32_t>(frame_banjo.x_pos),
      .y_pos = static_cast<int32_t>(frame_banjo.y_pos),
      .width = static_cast<int32_t>(frame_banjo.width),
      .height = static_cast<int32_t>(frame_banjo.height),
  };
}

fuchsia_hardware_display::wire::Frame ToFidlFrame(const Frame& frame) {
  ZX_DEBUG_ASSERT(frame.x_pos >= 0);
  ZX_DEBUG_ASSERT(frame.y_pos >= 0);
  ZX_DEBUG_ASSERT(frame.width >= 0);
  ZX_DEBUG_ASSERT(frame.height >= 0);
  return fuchsia_hardware_display::wire::Frame{
      .x_pos = static_cast<uint32_t>(frame.x_pos),
      .y_pos = static_cast<uint32_t>(frame.y_pos),
      .width = static_cast<uint32_t>(frame.width),
      .height = static_cast<uint32_t>(frame.height),
  };
}

frame_t ToBanjoFrame(const Frame& frame) {
  ZX_DEBUG_ASSERT(frame.x_pos >= 0);
  ZX_DEBUG_ASSERT(frame.y_pos >= 0);
  ZX_DEBUG_ASSERT(frame.width >= 0);
  ZX_DEBUG_ASSERT(frame.height >= 0);
  return frame_t{
      .x_pos = static_cast<uint32_t>(frame.x_pos),
      .y_pos = static_cast<uint32_t>(frame.y_pos),
      .width = static_cast<uint32_t>(frame.width),
      .height = static_cast<uint32_t>(frame.height),
  };
}

}  // namespace display
