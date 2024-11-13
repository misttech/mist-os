// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/transform.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

namespace display {

Transform ToTransform(
    fuchsia_hardware_display_types::wire::CoordinateTransformation transform_fidl) {
  return transform_fidl;
}

Transform ToTransform(coordinate_transformation_t transform_banjo) {
  switch (transform_banjo) {
    case COORDINATE_TRANSFORMATION_IDENTITY:
      return Transform::kIdentity;
    case COORDINATE_TRANSFORMATION_REFLECT_X:
      return Transform::kReflectX;
    case COORDINATE_TRANSFORMATION_REFLECT_Y:
      return Transform::kReflectY;
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_90:
      return Transform::kRotateCcw90;
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_180:
      return Transform::kRotateCcw180;
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_270:
      return Transform::kRotateCcw270;
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_X:
      return Transform::kRotateCcw90ReflectX;
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_Y:
      return Transform::kRotateCcw90ReflectY;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid banjo Transform %u", static_cast<int>(transform_banjo));
      return Transform::kIdentity;
  }
}

fuchsia_hardware_display_types::wire::CoordinateTransformation ToFidlTransform(
    Transform transform) {
  return transform;
}

coordinate_transformation_t ToBanjoFrameTransform(Transform transform) {
  switch (transform) {
    case Transform::kIdentity:
      return COORDINATE_TRANSFORMATION_IDENTITY;
    case Transform::kReflectX:
      return COORDINATE_TRANSFORMATION_REFLECT_X;
    case Transform::kReflectY:
      return COORDINATE_TRANSFORMATION_REFLECT_Y;
    case Transform::kRotateCcw90:
      return COORDINATE_TRANSFORMATION_ROTATE_CCW_90;
    case Transform::kRotateCcw180:
      return COORDINATE_TRANSFORMATION_ROTATE_CCW_180;
    case Transform::kRotateCcw270:
      return COORDINATE_TRANSFORMATION_ROTATE_CCW_270;
    case Transform::kRotateCcw90ReflectX:
      return COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_X;
    case Transform::kRotateCcw90ReflectY:
      return COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_Y;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid Transform %d", static_cast<int>(transform));
      return COORDINATE_TRANSFORMATION_IDENTITY;
  }
}

}  // namespace display
