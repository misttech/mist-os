// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/coordinate-transformation.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr CoordinateTransformation kReflectY2(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectY);

TEST(CoordinateTransformationTest, EqualityIsReflexive) {
  EXPECT_EQ(CoordinateTransformation::kReflectY, CoordinateTransformation::kReflectY);
  EXPECT_EQ(kReflectY2, kReflectY2);
  EXPECT_EQ(CoordinateTransformation::kReflectX, CoordinateTransformation::kReflectX);
}

TEST(CoordinateTransformationTest, EqualityIsSymmetric) {
  EXPECT_EQ(CoordinateTransformation::kReflectY, kReflectY2);
  EXPECT_EQ(kReflectY2, CoordinateTransformation::kReflectY);
}

TEST(CoordinateTransformationTest, EqualityForDifferentValues) {
  EXPECT_NE(CoordinateTransformation::kReflectY, CoordinateTransformation::kReflectX);
  EXPECT_NE(CoordinateTransformation::kReflectX, CoordinateTransformation::kReflectY);
  EXPECT_NE(kReflectY2, CoordinateTransformation::kReflectX);
  EXPECT_NE(CoordinateTransformation::kReflectX, kReflectY2);
}

TEST(CoordinateTransformationTest, ToBanjoCoordinateTransformation) {
  static constexpr coordinate_transformation_t banjo_transformation =
      CoordinateTransformation::kReflectY.ToBanjo();
  EXPECT_EQ(COORDINATE_TRANSFORMATION_REFLECT_Y, banjo_transformation);
}

TEST(CoordinateTransformationTest, ToFidlCoordinateTransformation) {
  static constexpr fuchsia_hardware_display_types::wire::CoordinateTransformation
      fidl_transformation = CoordinateTransformation::kReflectY.ToFidl();
  EXPECT_EQ(fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectY,
            fidl_transformation);
}

TEST(CoordinateTransformationTest, ToCoordinateTransformationWithBanjoValue) {
  static constexpr CoordinateTransformation transformation(COORDINATE_TRANSFORMATION_REFLECT_Y);
  EXPECT_EQ(CoordinateTransformation::kReflectY, transformation);
}

TEST(CoordinateTransformationTest, ToCoordinateTransformationWithFidlValue) {
  static constexpr CoordinateTransformation transformation(
      fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectY);
  EXPECT_EQ(CoordinateTransformation::kReflectY, transformation);
}

TEST(CoordinateTransformationTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(CoordinateTransformation::kReflectY,
            CoordinateTransformation(CoordinateTransformation::kReflectY.ToBanjo()));
  EXPECT_EQ(CoordinateTransformation::kReflectX,
            CoordinateTransformation(CoordinateTransformation::kReflectX.ToBanjo()));
}

TEST(CoordinateTransformationTest, FidlConversionRoundtrip) {
  EXPECT_EQ(CoordinateTransformation::kReflectY,
            CoordinateTransformation(CoordinateTransformation::kReflectY.ToFidl()));
  EXPECT_EQ(CoordinateTransformation::kReflectX,
            CoordinateTransformation(CoordinateTransformation::kReflectX.ToFidl()));
}

}  // namespace

}  // namespace display
