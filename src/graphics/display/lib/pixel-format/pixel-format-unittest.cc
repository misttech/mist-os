// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/pixel-format/pixel-format.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <zircon/errors.h>
#include <zircon/pixelformat.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

namespace display {

namespace {

TEST(AnyPixelFormatToImages2PixelFormat, ValidZxFormat) {
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_RGB_565);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kRgb565);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_RGB_332);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kRgb332);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_RGB_2220);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kRgb2220);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_ARGB_8888);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kBgra32);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_MONO_8);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kL8);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_I420);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kI420);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_NV12);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kNv12);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_RGB_888);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kBgr24);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_ABGR_8888);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kR8G8B8A8);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_ARGB_2_10_10_10);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kA2R10G10B10);
  }
  {
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(ZX_PIXEL_FORMAT_ABGR_2_10_10_10);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::PixelFormat::kA2B10G10R10);
  }
}

TEST(AnyPixelFormatToImages2PixelFormat, InvalidPixelFormat) {
  {
    // Invalid zx_pixel_format_t.
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(0x10303);
    ASSERT_TRUE(convert_result.is_error());
    EXPECT_EQ(convert_result.error_value(), ZX_ERR_INVALID_ARGS);
  }
  {
    // Invalid fuchsia.images2.PixelFormat.
    zx::result convert_result = AnyPixelFormatToImages2PixelFormat(0x999u);
    ASSERT_TRUE(convert_result.is_error());
    EXPECT_EQ(convert_result.error_value(), ZX_ERR_INVALID_ARGS);
  }
}

TEST(AnyPixelFormatToImages2PixelFormat, ValidSysmem2Format) {
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kR8G8B8A8;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kBgra32;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kI420;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kM420;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kNv12;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kYuy2;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kMjpeg;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kYv12;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kBgr24;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kRgb565;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kRgb332;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kRgb2220;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kL8;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kR8;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kR8G8;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kA2R10G10B10;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
  {
    const auto input_format = fuchsia_images2::wire::PixelFormat::kA2B10G10R10;
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormat(static_cast<AnyPixelFormat>(input_format));
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), input_format);
  }
}

TEST(AnyPixelFormatToImages2PixelFormatStdVector, ValidZxFormatsNoDuplicate) {
  std::vector<AnyPixelFormat> display_pixel_formats = {
      ZX_PIXEL_FORMAT_RGB_565,
      ZX_PIXEL_FORMAT_RGB_332,
      ZX_PIXEL_FORMAT_RGB_2220,
      ZX_PIXEL_FORMAT_ARGB_8888,
      ZX_PIXEL_FORMAT_MONO_8,
      ZX_PIXEL_FORMAT_I420,
      ZX_PIXEL_FORMAT_NV12,
      ZX_PIXEL_FORMAT_RGB_888,
      ZX_PIXEL_FORMAT_ABGR_8888,
      ZX_PIXEL_FORMAT_ARGB_2_10_10_10,
      ZX_PIXEL_FORMAT_ABGR_2_10_10_10,
  };
  const std::vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kRgb565,      fuchsia_images2::PixelFormat::kRgb332,
      fuchsia_images2::PixelFormat::kRgb2220,     fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kL8,          fuchsia_images2::PixelFormat::kI420,
      fuchsia_images2::PixelFormat::kNv12,        fuchsia_images2::PixelFormat::kBgr24,
      fuchsia_images2::PixelFormat::kR8G8B8A8,    fuchsia_images2::PixelFormat::kA2R10G10B10,
      fuchsia_images2::PixelFormat::kA2B10G10R10,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatStdVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  std::vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_THAT(actual_sysmem_pixel_formats, testing::ContainerEq(expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatStdVector, ValidZxFormatsDeduplicate) {
  std::vector<AnyPixelFormat> display_pixel_formats = {
      ZX_PIXEL_FORMAT_RGB_x888,  ZX_PIXEL_FORMAT_RGB_565, ZX_PIXEL_FORMAT_ARGB_8888,
      ZX_PIXEL_FORMAT_ABGR_8888, ZX_PIXEL_FORMAT_RGB_888, ZX_PIXEL_FORMAT_BGR_888x,
  };
  const std::vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatStdVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  std::vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_THAT(actual_sysmem_pixel_formats, testing::ContainerEq(expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatStdVector, ValidSysmem2Formats) {
  std::vector<AnyPixelFormat> display_pixel_formats = {
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kRgb565),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kR8G8B8A8),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgr24),
  };
  const std::vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatStdVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  std::vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_THAT(actual_sysmem_pixel_formats, testing::ContainerEq(expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatStdVector, ValidSysmem2FormatsDeduplicate) {
  std::vector<AnyPixelFormat> display_pixel_formats = {
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kRgb565),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kR8G8B8A8),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgr24),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
  };
  const std::vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatStdVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  std::vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_THAT(actual_sysmem_pixel_formats, testing::ContainerEq(expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatStdVector, ValidMixedFormats) {
  std::vector<AnyPixelFormat> display_pixel_formats = {
      ZX_PIXEL_FORMAT_ARGB_8888,
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kRgb565),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kR8G8B8A8),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgr24),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
  };
  const std::vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatStdVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  std::vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_THAT(actual_sysmem_pixel_formats, testing::ContainerEq(expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatStdVector, InvalidFormat) {
  {
    // Invalid zx_pixel_format_t values.
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormatStdVector(std::vector<AnyPixelFormat>{
            0x10303,
            0x20606,
        });
    ASSERT_TRUE(convert_result.is_error());
    EXPECT_EQ(convert_result.error_value(), ZX_ERR_INVALID_ARGS);
  }

  {
    // Invalid fuchsia.images2.PixelFormat.
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormatStdVector(std::vector<AnyPixelFormat>{
            0x999,
            0x888,
        });
    ASSERT_TRUE(convert_result.is_error());
    EXPECT_EQ(convert_result.error_value(), ZX_ERR_INVALID_ARGS);
  }

  {
    // Mixture of valid values and invalid values.
    zx::result convert_result =
        AnyPixelFormatToImages2PixelFormatStdVector(std::vector<AnyPixelFormat>{
            ZX_PIXEL_FORMAT_RGB_x888,
            0x10303,
        });
    ASSERT_TRUE(convert_result.is_error());
    EXPECT_EQ(convert_result.error_value(), ZX_ERR_INVALID_ARGS);
  }
}

// testing::ContainerEq() doesn't work for fbl::Vector types which doesn't allow
// copying, so we have to implement the equality check ourselves.
bool Equal(const fbl::Vector<fuchsia_images2::wire::PixelFormat>& lhs,
           const fbl::Vector<fuchsia_images2::wire::PixelFormat>& rhs) {
  if (lhs.size() != rhs.size()) {
    return false;
  }
  return std::equal(lhs.begin(), lhs.end(), rhs.begin());
}

TEST(AnyPixelFormatToImages2PixelFormatFblVector, ValidZxFormatsNoDuplicate) {
  fbl::Vector<AnyPixelFormat> display_pixel_formats = {
      ZX_PIXEL_FORMAT_RGB_565,
      ZX_PIXEL_FORMAT_RGB_332,
      ZX_PIXEL_FORMAT_RGB_2220,
      ZX_PIXEL_FORMAT_ARGB_8888,
      ZX_PIXEL_FORMAT_MONO_8,
      ZX_PIXEL_FORMAT_I420,
      ZX_PIXEL_FORMAT_NV12,
      ZX_PIXEL_FORMAT_RGB_888,
      ZX_PIXEL_FORMAT_ABGR_8888,
      ZX_PIXEL_FORMAT_ARGB_2_10_10_10,
      ZX_PIXEL_FORMAT_ABGR_2_10_10_10,
  };
  const fbl::Vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kRgb565,      fuchsia_images2::PixelFormat::kRgb332,
      fuchsia_images2::PixelFormat::kRgb2220,     fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kL8,          fuchsia_images2::PixelFormat::kI420,
      fuchsia_images2::PixelFormat::kNv12,        fuchsia_images2::PixelFormat::kBgr24,
      fuchsia_images2::PixelFormat::kR8G8B8A8,    fuchsia_images2::PixelFormat::kA2R10G10B10,
      fuchsia_images2::PixelFormat::kA2B10G10R10,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatFblVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  fbl::Vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_TRUE(Equal(actual_sysmem_pixel_formats, expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatFblVector, ValidZxFormatsDeduplicate) {
  fbl::Vector<AnyPixelFormat> display_pixel_formats = {
      ZX_PIXEL_FORMAT_RGB_x888,  ZX_PIXEL_FORMAT_RGB_565, ZX_PIXEL_FORMAT_ARGB_8888,
      ZX_PIXEL_FORMAT_ABGR_8888, ZX_PIXEL_FORMAT_RGB_888, ZX_PIXEL_FORMAT_BGR_888x,
  };
  const fbl::Vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatFblVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  fbl::Vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_TRUE(Equal(actual_sysmem_pixel_formats, expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatFblVector, ValidSysmem2Formats) {
  fbl::Vector<AnyPixelFormat> display_pixel_formats = {
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kRgb565),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kR8G8B8A8),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgr24),
  };
  const fbl::Vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatFblVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  fbl::Vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_TRUE(Equal(actual_sysmem_pixel_formats, expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatFblVector, ValidSysmem2FormatsDeduplicate) {
  fbl::Vector<AnyPixelFormat> display_pixel_formats = {
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kRgb565),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kR8G8B8A8),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgr24),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
  };
  const fbl::Vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatFblVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  fbl::Vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_TRUE(Equal(actual_sysmem_pixel_formats, expected_sysmem_pixel_formats));
}

TEST(AnyPixelFormatToImages2PixelFormatFblVector, ValidMixedFormats) {
  fbl::Vector<AnyPixelFormat> display_pixel_formats = {
      ZX_PIXEL_FORMAT_ARGB_8888,
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kRgb565),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kR8G8B8A8),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgr24),
      static_cast<AnyPixelFormat>(fuchsia_images2::PixelFormat::kBgra32),
  };
  const fbl::Vector<fuchsia_images2::wire::PixelFormat> expected_sysmem_pixel_formats = {
      fuchsia_images2::PixelFormat::kBgra32,
      fuchsia_images2::PixelFormat::kRgb565,
      fuchsia_images2::PixelFormat::kR8G8B8A8,
      fuchsia_images2::PixelFormat::kBgr24,
  };
  zx::result convert_result = AnyPixelFormatToImages2PixelFormatFblVector(display_pixel_formats);
  ASSERT_TRUE(convert_result.is_ok());
  fbl::Vector<fuchsia_images2::wire::PixelFormat> actual_sysmem_pixel_formats =
      std::move(convert_result.value());
  EXPECT_TRUE(Equal(actual_sysmem_pixel_formats, expected_sysmem_pixel_formats));
}

}  // namespace

}  // namespace display
