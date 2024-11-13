// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr ImageTilingType kLinear2(fuchsia_hardware_display_types::wire::kImageTilingTypeLinear);

TEST(ImageTilingTypeTest, EqualityIsReflexive) {
  EXPECT_EQ(kImageTilingTypeLinear, kImageTilingTypeLinear);
  EXPECT_EQ(kLinear2, kLinear2);
  EXPECT_EQ(kImageTilingTypeCapture, kImageTilingTypeCapture);
}

TEST(ImageTilingTypeTest, EqualityIsSymmetric) {
  EXPECT_EQ(kImageTilingTypeLinear, kLinear2);
  EXPECT_EQ(kLinear2, kImageTilingTypeLinear);
}

TEST(ImageTilingTypeTest, EqualityForDifferentValues) {
  EXPECT_NE(kImageTilingTypeLinear, kImageTilingTypeCapture);
  EXPECT_NE(kImageTilingTypeCapture, kImageTilingTypeLinear);
  EXPECT_NE(kLinear2, kImageTilingTypeCapture);
  EXPECT_NE(kImageTilingTypeCapture, kLinear2);
}

TEST(ImageTilingTypeTest, ToBanjoImageTilingType) {
  EXPECT_EQ(IMAGE_TILING_TYPE_LINEAR, kImageTilingTypeLinear.ToBanjo());
  EXPECT_EQ(IMAGE_TILING_TYPE_CAPTURE, kImageTilingTypeCapture.ToBanjo());
}

TEST(ImageTilingTypeTest, ToFidlImageTilingType) {
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kImageTilingTypeLinear,
            kImageTilingTypeLinear.ToFidl());
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
            kImageTilingTypeCapture.ToFidl());
}

TEST(ImageTilingTypeTest, ToImageTilingTypeWithBanjoValue) {
  EXPECT_EQ(kImageTilingTypeLinear, ImageTilingType(IMAGE_TILING_TYPE_LINEAR));
  EXPECT_EQ(kImageTilingTypeCapture, ImageTilingType(IMAGE_TILING_TYPE_CAPTURE));
}

TEST(ImageTilingTypeTest, ToImageTilingTypeWithFidlValue) {
  EXPECT_EQ(kImageTilingTypeLinear,
            ImageTilingType(fuchsia_hardware_display_types::wire::kImageTilingTypeLinear));
  EXPECT_EQ(kImageTilingTypeCapture,
            ImageTilingType(fuchsia_hardware_display_types::wire::kImageTilingTypeCapture));
}

TEST(ImageTilingTypeTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kImageTilingTypeLinear, ImageTilingType(kImageTilingTypeLinear.ToBanjo()));
  EXPECT_EQ(kImageTilingTypeCapture, ImageTilingType(kImageTilingTypeCapture.ToBanjo()));
}

TEST(ImageTilingTypeTest, FidlConversionRoundtrip) {
  EXPECT_EQ(kImageTilingTypeLinear, ImageTilingType(kImageTilingTypeLinear.ToFidl()));
  EXPECT_EQ(kImageTilingTypeCapture, ImageTilingType(kImageTilingTypeCapture.ToFidl()));
}

}  // namespace

}  // namespace display
