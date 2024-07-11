// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/frame.h"

#include <fidl/fuchsia.math/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <zircon/assert.h>

#include <limits>

namespace display {

Frame ToFrame(const fuchsia_math::wire::RectU& rectangle_fidl) {
  ZX_DEBUG_ASSERT(rectangle_fidl.x <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(rectangle_fidl.y <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(rectangle_fidl.width <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(rectangle_fidl.height <= std::numeric_limits<int32_t>::max());
  return Frame{
      .x_pos = static_cast<int32_t>(rectangle_fidl.x),
      .y_pos = static_cast<int32_t>(rectangle_fidl.y),
      .width = static_cast<int32_t>(rectangle_fidl.width),
      .height = static_cast<int32_t>(rectangle_fidl.height),
  };
}

Frame ToFrame(const rect_u_t& rectangle_banjo) {
  ZX_DEBUG_ASSERT(rectangle_banjo.x <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(rectangle_banjo.y <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(rectangle_banjo.width <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(rectangle_banjo.height <= std::numeric_limits<int32_t>::max());
  return Frame{
      .x_pos = static_cast<int32_t>(rectangle_banjo.x),
      .y_pos = static_cast<int32_t>(rectangle_banjo.y),
      .width = static_cast<int32_t>(rectangle_banjo.width),
      .height = static_cast<int32_t>(rectangle_banjo.height),
  };
}

fuchsia_math::wire::RectU ToFidlFrame(const Frame& frame) {
  ZX_DEBUG_ASSERT(frame.x_pos >= 0);
  ZX_DEBUG_ASSERT(frame.y_pos >= 0);
  ZX_DEBUG_ASSERT(frame.width >= 0);
  ZX_DEBUG_ASSERT(frame.height >= 0);
  return fuchsia_math::wire::RectU{
      .x = static_cast<uint32_t>(frame.x_pos),
      .y = static_cast<uint32_t>(frame.y_pos),
      .width = static_cast<uint32_t>(frame.width),
      .height = static_cast<uint32_t>(frame.height),
  };
}

rect_u_t ToBanjoFrame(const Frame& frame) {
  ZX_DEBUG_ASSERT(frame.x_pos >= 0);
  ZX_DEBUG_ASSERT(frame.y_pos >= 0);
  ZX_DEBUG_ASSERT(frame.width >= 0);
  ZX_DEBUG_ASSERT(frame.height >= 0);
  return rect_u_t{
      .x = static_cast<uint32_t>(frame.x_pos),
      .y = static_cast<uint32_t>(frame.y_pos),
      .width = static_cast<uint32_t>(frame.width),
      .height = static_cast<uint32_t>(frame.height),
  };
}

}  // namespace display
