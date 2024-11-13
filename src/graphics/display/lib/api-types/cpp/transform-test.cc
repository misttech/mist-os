// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/transform.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(Transform, FidlConversion) {
  EXPECT_EQ(ToTransform(fuchsia_hardware_display_types::wire::CoordinateTransformation::kIdentity),
            Transform::kIdentity);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectX),
            Transform::kReflectX);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectY),
            Transform::kReflectY);
  EXPECT_EQ(
      ToTransform(fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90),
      Transform::kRotateCcw90);
  EXPECT_EQ(
      ToTransform(fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw180),
      Transform::kRotateCcw180);
  EXPECT_EQ(
      ToTransform(fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw270),
      Transform::kRotateCcw270);
  EXPECT_EQ(
      ToTransform(
          fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectX),
      Transform::kRotateCcw90ReflectX);
  EXPECT_EQ(
      ToTransform(
          fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectY),
      Transform::kRotateCcw90ReflectY);

  EXPECT_EQ(ToFidlTransform(Transform::kIdentity),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kIdentity);
  EXPECT_EQ(ToFidlTransform(Transform::kReflectX),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectX);
  EXPECT_EQ(ToFidlTransform(Transform::kReflectY),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectY);
  EXPECT_EQ(ToFidlTransform(Transform::kRotateCcw90),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90);
  EXPECT_EQ(ToFidlTransform(Transform::kRotateCcw180),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw180);
  EXPECT_EQ(ToFidlTransform(Transform::kRotateCcw270),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw270);
  EXPECT_EQ(ToFidlTransform(Transform::kRotateCcw90ReflectX),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectX);
  EXPECT_EQ(ToFidlTransform(Transform::kRotateCcw90ReflectY),
            fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectY);
}

TEST(Transform, BanjoConversion) {
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_IDENTITY), Transform::kIdentity);
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_REFLECT_X), Transform::kReflectX);
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_REFLECT_Y), Transform::kReflectY);
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_ROTATE_CCW_90), Transform::kRotateCcw90);
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_ROTATE_CCW_180), Transform::kRotateCcw180);
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_ROTATE_CCW_270), Transform::kRotateCcw270);
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_X),
            Transform::kRotateCcw90ReflectX);
  EXPECT_EQ(ToTransform(COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_Y),
            Transform::kRotateCcw90ReflectY);

  EXPECT_EQ(ToBanjoFrameTransform(Transform::kIdentity), COORDINATE_TRANSFORMATION_IDENTITY);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kReflectX), COORDINATE_TRANSFORMATION_REFLECT_X);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kReflectY), COORDINATE_TRANSFORMATION_REFLECT_Y);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRotateCcw90),
            COORDINATE_TRANSFORMATION_ROTATE_CCW_90);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRotateCcw180),
            COORDINATE_TRANSFORMATION_ROTATE_CCW_180);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRotateCcw270),
            COORDINATE_TRANSFORMATION_ROTATE_CCW_270);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRotateCcw90ReflectX),
            COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_X);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRotateCcw90ReflectY),
            COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_Y);
}

}  // namespace

}  // namespace display
