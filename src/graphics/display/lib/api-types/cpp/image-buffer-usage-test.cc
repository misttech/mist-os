// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

namespace display {

namespace {

constexpr ImageBufferUsage kDisplayUsage = {
    .tiling_type = kImageTilingTypeLinear,
};
constexpr ImageBufferUsage kDisplayUsage2 = {
    .tiling_type = kImageTilingTypeLinear,
};
constexpr ImageBufferUsage kCaptureUsage = {
    .tiling_type = kImageTilingTypeCapture,
};

TEST(ImageBufferUsageTest, EqualityIsReflexive) {
  EXPECT_EQ(kDisplayUsage, kDisplayUsage);
  EXPECT_EQ(kDisplayUsage2, kDisplayUsage2);
  EXPECT_EQ(kCaptureUsage, kCaptureUsage);
}

TEST(ImageBufferUsageTest, EqualityIsSymmetric) {
  EXPECT_EQ(kDisplayUsage, kDisplayUsage2);
  EXPECT_EQ(kDisplayUsage2, kDisplayUsage);
}

TEST(ImageBufferUsageTest, EqualityForDifferentTilingTypes) {
  EXPECT_NE(kDisplayUsage, kCaptureUsage);
  EXPECT_NE(kCaptureUsage, kDisplayUsage);
}

TEST(ImageBufferUsageTest, FromFidlImageBufferUsage) {
  static constexpr fuchsia_hardware_display_types::wire::ImageBufferUsage fidl_image_buffer_usage =
      {
          .tiling_type = fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
      };

  static constexpr ImageBufferUsage image_buffer_usage =
      ToImageBufferUsage(fidl_image_buffer_usage);
  EXPECT_EQ(kImageTilingTypeCapture, image_buffer_usage.tiling_type);
}

TEST(ImageBufferUsageTest, FromBanjoImageBufferUsage) {
  static constexpr image_buffer_usage_t banjo_image_buffer_usage = {
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  };

  static constexpr ImageBufferUsage image_buffer_usage =
      ToImageBufferUsage(banjo_image_buffer_usage);
  EXPECT_EQ(kImageTilingTypeCapture, image_buffer_usage.tiling_type);
}

TEST(ImageBufferUsageTest, ToFidlImageBufferUsage) {
  static constexpr fuchsia_hardware_display_types::wire::ImageBufferUsage fidl_image_buffer_usage =
      ToFidlImageBufferUsage(kCaptureUsage);
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
            fidl_image_buffer_usage.tiling_type);
}

TEST(ImageBufferUsageTest, ToBanjoImageBufferUsage) {
  static constexpr image_buffer_usage_t banjo_image_buffer_usage =
      ToBanjoImageBufferUsage(kCaptureUsage);
  EXPECT_EQ(IMAGE_TILING_TYPE_CAPTURE, banjo_image_buffer_usage.tiling_type);
}

TEST(ImageBufferUsageTest, FidlDisplayIdConversionRoundtrip) {
  EXPECT_EQ(kDisplayUsage, ToImageBufferUsage(ToFidlImageBufferUsage(kDisplayUsage)));
  EXPECT_EQ(kCaptureUsage, ToImageBufferUsage(ToFidlImageBufferUsage(kCaptureUsage)));
}

TEST(ImageBufferUsageTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kDisplayUsage, ToImageBufferUsage(ToBanjoImageBufferUsage(kDisplayUsage)));
  EXPECT_EQ(kCaptureUsage, ToImageBufferUsage(ToBanjoImageBufferUsage(kCaptureUsage)));
}

}  // namespace

}  // namespace display
