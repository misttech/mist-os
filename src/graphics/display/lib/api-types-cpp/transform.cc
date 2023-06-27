// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/transform.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <zircon/assert.h>

#include <limits>
#include <type_traits>

namespace display {

Transform ToTransform(fuchsia_hardware_display::wire::Transform transform_fidl) {
  return transform_fidl;
}

Transform ToTransform(frame_transform_t frame_transform_banjo) {
  switch (frame_transform_banjo) {
    case FRAME_TRANSFORM_IDENTITY:
      return Transform::kIdentity;
    case FRAME_TRANSFORM_REFLECT_X:
      return Transform::kReflectX;
    case FRAME_TRANSFORM_REFLECT_Y:
      return Transform::kReflectY;
    case FRAME_TRANSFORM_ROT_90:
      return Transform::kRot90;
    case FRAME_TRANSFORM_ROT_180:
      return Transform::kRot180;
    case FRAME_TRANSFORM_ROT_270:
      return Transform::kRot270;
    case FRAME_TRANSFORM_ROT_90_REFLECT_X:
      return Transform::kRot90ReflectX;
    case FRAME_TRANSFORM_ROT_90_REFLECT_Y:
      return Transform::kRot90ReflectY;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid banjo Transform %u",
                          static_cast<int>(frame_transform_banjo));
      return Transform::kIdentity;
  }
}

fuchsia_hardware_display::wire::Transform ToFidlTransform(Transform transform) { return transform; }

frame_transform_t ToBanjoFrameTransform(Transform transform) {
  switch (transform) {
    case Transform::kIdentity:
      return FRAME_TRANSFORM_IDENTITY;
    case Transform::kReflectX:
      return FRAME_TRANSFORM_REFLECT_X;
    case Transform::kReflectY:
      return FRAME_TRANSFORM_REFLECT_Y;
    case Transform::kRot90:
      return FRAME_TRANSFORM_ROT_90;
    case Transform::kRot180:
      return FRAME_TRANSFORM_ROT_180;
    case Transform::kRot270:
      return FRAME_TRANSFORM_ROT_270;
    case Transform::kRot90ReflectX:
      return FRAME_TRANSFORM_ROT_90_REFLECT_X;
    case Transform::kRot90ReflectY:
      return FRAME_TRANSFORM_ROT_90_REFLECT_Y;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid Transform %d", static_cast<int>(transform));
      return FRAME_TRANSFORM_IDENTITY;
  }
}

}  // namespace display
