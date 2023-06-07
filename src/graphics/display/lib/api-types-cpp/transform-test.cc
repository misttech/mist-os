// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/transform.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(Transform, FidlConversion) {
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kIdentity),
            Transform::kIdentity);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kReflectX),
            Transform::kReflectX);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kReflectY),
            Transform::kReflectY);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kRot90), Transform::kRot90);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kRot180), Transform::kRot180);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kRot270), Transform::kRot270);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kRot90ReflectX),
            Transform::kRot90ReflectX);
  EXPECT_EQ(ToTransform(fuchsia_hardware_display::wire::Transform::kRot90ReflectY),
            Transform::kRot90ReflectY);

  EXPECT_EQ(ToFidlTransform(Transform::kIdentity),
            fuchsia_hardware_display::wire::Transform::kIdentity);
  EXPECT_EQ(ToFidlTransform(Transform::kReflectX),
            fuchsia_hardware_display::wire::Transform::kReflectX);
  EXPECT_EQ(ToFidlTransform(Transform::kReflectY),
            fuchsia_hardware_display::wire::Transform::kReflectY);
  EXPECT_EQ(ToFidlTransform(Transform::kRot90), fuchsia_hardware_display::wire::Transform::kRot90);
  EXPECT_EQ(ToFidlTransform(Transform::kRot180),
            fuchsia_hardware_display::wire::Transform::kRot180);
  EXPECT_EQ(ToFidlTransform(Transform::kRot270),
            fuchsia_hardware_display::wire::Transform::kRot270);
  EXPECT_EQ(ToFidlTransform(Transform::kRot90ReflectX),
            fuchsia_hardware_display::wire::Transform::kRot90ReflectX);
  EXPECT_EQ(ToFidlTransform(Transform::kRot90ReflectY),
            fuchsia_hardware_display::wire::Transform::kRot90ReflectY);
}

TEST(Transform, BanjoConversion) {
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_IDENTITY), Transform::kIdentity);
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_REFLECT_X), Transform::kReflectX);
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_REFLECT_Y), Transform::kReflectY);
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_ROT_90), Transform::kRot90);
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_ROT_180), Transform::kRot180);
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_ROT_270), Transform::kRot270);
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_ROT_90_REFLECT_X), Transform::kRot90ReflectX);
  EXPECT_EQ(ToTransform(FRAME_TRANSFORM_ROT_90_REFLECT_Y), Transform::kRot90ReflectY);

  EXPECT_EQ(ToBanjoFrameTransform(Transform::kIdentity), FRAME_TRANSFORM_IDENTITY);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kReflectX), FRAME_TRANSFORM_REFLECT_X);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kReflectY), FRAME_TRANSFORM_REFLECT_Y);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRot90), FRAME_TRANSFORM_ROT_90);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRot180), FRAME_TRANSFORM_ROT_180);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRot270), FRAME_TRANSFORM_ROT_270);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRot90ReflectX), FRAME_TRANSFORM_ROT_90_REFLECT_X);
  EXPECT_EQ(ToBanjoFrameTransform(Transform::kRot90ReflectY), FRAME_TRANSFORM_ROT_90_REFLECT_Y);
}

}  // namespace

}  // namespace display
