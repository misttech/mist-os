// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/engine/engine_types.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.math/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

namespace {

using fuchsia::ui::composition::Orientation;
using fuchsia_ui_composition::ImageFlip;

}  // namespace
namespace flatland {

DisplaySrcDstFrames DisplaySrcDstFrames::New(ImageRect rectangle, allocation::ImageMetadata image) {
  fuchsia_math::wire::RectU image_source = {
      .x = static_cast<uint32_t>(rectangle.texel_uvs[0].x),
      .y = static_cast<uint32_t>(rectangle.texel_uvs[0].y),
      .width = static_cast<uint32_t>(rectangle.texel_uvs[2].x - rectangle.texel_uvs[0].x),
      .height = static_cast<uint32_t>(rectangle.texel_uvs[2].y - rectangle.texel_uvs[0].y),
  };

  fuchsia_math::wire::RectU display_destination = {
      .x = static_cast<uint32_t>(rectangle.origin.x),
      .y = static_cast<uint32_t>(rectangle.origin.y),
      .width = static_cast<uint32_t>(rectangle.extent.x),
      .height = static_cast<uint32_t>(rectangle.extent.y),
  };
  return {.src = image_source, .dst = display_destination};
}

fuchsia_hardware_display_types::CoordinateTransformation GetDisplayTransformFromOrientationAndFlip(
    Orientation orientation, ImageFlip image_flip) {
  // For flatland, image flips occur before any parent Transform geometric attributes (such as
  // rotation). However, for the display controller, the reflection specified in the Transform is
  // applied after rotation. The flatland transformations must be converted to the equivalent
  // display controller transform.
  switch (orientation) {
    case Orientation::CCW_0_DEGREES:
      switch (image_flip) {
        case ImageFlip::kNone:
          return fuchsia_hardware_display_types::CoordinateTransformation::kIdentity;
        case ImageFlip::kLeftRight:
          return fuchsia_hardware_display_types::CoordinateTransformation::kReflectY;
        case ImageFlip::kUpDown:
          return fuchsia_hardware_display_types::CoordinateTransformation::kReflectX;
      }

    case Orientation::CCW_90_DEGREES:
      switch (image_flip) {
        case ImageFlip::kNone:
          return fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90;
        case ImageFlip::kLeftRight:
          // Left-right flip + 90Ccw is equivalent to 90Ccw + up-down flip.
          return fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectX;
        case ImageFlip::kUpDown:
          // Up-down flip + 90Ccw is equivalent to 90Ccw + left-right flip.
          return fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectY;
      }

    case Orientation::CCW_180_DEGREES:
      switch (image_flip) {
        case ImageFlip::kNone:
          return fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw180;
        case ImageFlip::kLeftRight:
          // Left-right flip + 180 degree rotation is equivalent to up-down flip.
          return fuchsia_hardware_display_types::CoordinateTransformation::kReflectX;
        case ImageFlip::kUpDown:
          // Up-down flip + 180 degree rotation is equivalent to left-right flip.
          return fuchsia_hardware_display_types::CoordinateTransformation::kReflectY;
      }

    case Orientation::CCW_270_DEGREES:
      switch (image_flip) {
        case ImageFlip::kNone:
          return fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw270;
        case ImageFlip::kLeftRight:
          // Left-right flip + 270Ccw is equivalent to 270Ccw + up-down flip, which in turn is
          // equivalent to 90Ccw + left-right flip.
          return fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectY;
        case ImageFlip::kUpDown:
          // Up-down flip + 270Ccw is equivalent to 270Ccw + left-right flip, which in turn is
          // equivalent to 90Ccw + up-down flip.
          return fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectX;
      }
  }

  FX_NOTREACHED();
}

}  // namespace flatland
