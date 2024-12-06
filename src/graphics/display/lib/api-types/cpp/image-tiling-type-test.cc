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
  EXPECT_EQ(ImageTilingType::kLinear, ImageTilingType::kLinear);
  EXPECT_EQ(kLinear2, kLinear2);
  EXPECT_EQ(ImageTilingType::kCapture, ImageTilingType::kCapture);
}

TEST(ImageTilingTypeTest, EqualityIsSymmetric) {
  EXPECT_EQ(ImageTilingType::kLinear, kLinear2);
  EXPECT_EQ(kLinear2, ImageTilingType::kLinear);
}

TEST(ImageTilingTypeTest, EqualityForDifferentValues) {
  EXPECT_NE(ImageTilingType::kLinear, ImageTilingType::kCapture);
  EXPECT_NE(ImageTilingType::kCapture, ImageTilingType::kLinear);
  EXPECT_NE(kLinear2, ImageTilingType::kCapture);
  EXPECT_NE(ImageTilingType::kCapture, kLinear2);
}

TEST(ImageTilingTypeTest, ToBanjoImageTilingType) {
  EXPECT_EQ(IMAGE_TILING_TYPE_LINEAR, ImageTilingType::kLinear.ToBanjo());
  EXPECT_EQ(IMAGE_TILING_TYPE_CAPTURE, ImageTilingType::kCapture.ToBanjo());
}

TEST(ImageTilingTypeTest, ToFidlImageTilingType) {
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kImageTilingTypeLinear,
            ImageTilingType::kLinear.ToFidl());
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
            ImageTilingType::kCapture.ToFidl());
}

TEST(ImageTilingTypeTest, ToImageTilingTypeWithBanjoValue) {
  EXPECT_EQ(ImageTilingType::kLinear, ImageTilingType(IMAGE_TILING_TYPE_LINEAR));
  EXPECT_EQ(ImageTilingType::kCapture, ImageTilingType(IMAGE_TILING_TYPE_CAPTURE));
}

TEST(ImageTilingTypeTest, ToImageTilingTypeWithFidlValue) {
  EXPECT_EQ(ImageTilingType::kLinear,
            ImageTilingType(fuchsia_hardware_display_types::wire::kImageTilingTypeLinear));
  EXPECT_EQ(ImageTilingType::kCapture,
            ImageTilingType(fuchsia_hardware_display_types::wire::kImageTilingTypeCapture));
}

TEST(ImageTilingTypeTest, ValueForLogging) {
  EXPECT_EQ(static_cast<uint32_t>(fuchsia_hardware_display_types::wire::kImageTilingTypeLinear),
            ImageTilingType::kLinear.ValueForLogging());
  EXPECT_EQ(static_cast<uint32_t>(fuchsia_hardware_display_types::wire::kImageTilingTypeCapture),
            ImageTilingType::kCapture.ValueForLogging());
}

TEST(ImageTilingTypeTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(ImageTilingType::kLinear, ImageTilingType(ImageTilingType::kLinear.ToBanjo()));
  EXPECT_EQ(ImageTilingType::kCapture, ImageTilingType(ImageTilingType::kCapture.ToBanjo()));
}

TEST(ImageTilingTypeTest, FidlConversionRoundtrip) {
  EXPECT_EQ(ImageTilingType::kLinear, ImageTilingType(ImageTilingType::kLinear.ToFidl()));
  EXPECT_EQ(ImageTilingType::kCapture, ImageTilingType(ImageTilingType::kCapture.ToFidl()));
}

}  // namespace

}  // namespace display
