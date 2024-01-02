// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/engine/engine_types.h"

#include <lib/syslog/cpp/macros.h>

namespace {

using fuchsia::ui::composition::ImageFlip;
using fuchsia::ui::composition::Orientation;
using fhd_Transform = fuchsia::hardware::display::types::Transform;

}  // namespace
namespace flatland {

DisplaySrcDstFrames DisplaySrcDstFrames::New(ImageRect rectangle, allocation::ImageMetadata image) {
  fuchsia::hardware::display::types::Frame src_frame = {
      .x_pos = static_cast<uint32_t>(rectangle.texel_uvs[0].x),
      .y_pos = static_cast<uint32_t>(rectangle.texel_uvs[0].y),
      .width = static_cast<uint32_t>(rectangle.texel_uvs[2].x - rectangle.texel_uvs[0].x),
      .height = static_cast<uint32_t>(rectangle.texel_uvs[2].y - rectangle.texel_uvs[0].y),
  };

  fuchsia::hardware::display::types::Frame dst_frame = {
      .x_pos = static_cast<uint32_t>(rectangle.origin.x),
      .y_pos = static_cast<uint32_t>(rectangle.origin.y),
      .width = static_cast<uint32_t>(rectangle.extent.x),
      .height = static_cast<uint32_t>(rectangle.extent.y),
  };
  return {.src = src_frame, .dst = dst_frame};
}

fuchsia::hardware::display::types::Transform GetDisplayTransformFromOrientationAndFlip(
    Orientation orientation, ImageFlip image_flip) {
  // For flatland, image flips occur before any parent Transform geometric attributes (such as
  // rotation). However, for the display controller, the reflection specified in the Transform is
  // applied after rotation. The flatland transformations must be converted to the equivalent
  // display controller transform.
  switch (orientation) {
    case Orientation::CCW_0_DEGREES:
      switch (image_flip) {
        case ImageFlip::NONE:
          return fhd_Transform::IDENTITY;
        case ImageFlip::LEFT_RIGHT:
          return fhd_Transform::REFLECT_Y;
        case ImageFlip::UP_DOWN:
          return fhd_Transform::REFLECT_X;
      }

    case Orientation::CCW_90_DEGREES:
      switch (image_flip) {
        case ImageFlip::NONE:
          return fhd_Transform::ROT_90;
        case ImageFlip::LEFT_RIGHT:
          // Left-right flip + 90Ccw is equivalent to 90Ccw + up-down flip.
          return fhd_Transform::ROT_90_REFLECT_X;
        case ImageFlip::UP_DOWN:
          // Up-down flip + 90Ccw is equivalent to 90Ccw + left-right flip.
          return fhd_Transform::ROT_90_REFLECT_Y;
      }

    case Orientation::CCW_180_DEGREES:
      switch (image_flip) {
        case ImageFlip::NONE:
          return fhd_Transform::ROT_180;
        case ImageFlip::LEFT_RIGHT:
          // Left-right flip + 180 degree rotation is equivalent to up-down flip.
          return fhd_Transform::REFLECT_X;
        case ImageFlip::UP_DOWN:
          // Up-down flip + 180 degree rotation is equivalent to left-right flip.
          return fhd_Transform::REFLECT_Y;
      }

    case Orientation::CCW_270_DEGREES:
      switch (image_flip) {
        case ImageFlip::NONE:
          return fhd_Transform::ROT_270;
        case ImageFlip::LEFT_RIGHT:
          // Left-right flip + 270Ccw is equivalent to 270Ccw + up-down flip, which in turn is
          // equivalent to 90Ccw + left-right flip.
          return fhd_Transform::ROT_90_REFLECT_Y;
        case ImageFlip::UP_DOWN:
          // Up-down flip + 270Ccw is equivalent to 270Ccw + left-right flip, which in turn is
          // equivalent to 90Ccw + up-down flip.
          return fhd_Transform::ROT_90_REFLECT_X;
      }
  }

  FX_NOTREACHED();
}

}  // namespace flatland
