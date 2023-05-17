// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/image-format/image_format.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zbi-format/graphics.h>

#include <fbl/array.h>
#include <zxtest/zxtest.h>

namespace sysmem_v1 = fuchsia_sysmem;
namespace sysmem_v2 = fuchsia_sysmem2;

TEST(ImageFormat, LinearComparison_V2) {
  PixelFormatAndModifier plain;
  plain.pixel_format = fuchsia_images2::PixelFormat::kBgra32;

  PixelFormatAndModifier linear;
  linear.pixel_format = fuchsia_images2::PixelFormat::kBgra32;
  linear.pixel_format_modifier = fuchsia_images2::kFormatModifierLinear;

  PixelFormatAndModifier x_tiled;
  x_tiled.pixel_format = fuchsia_images2::PixelFormat::kBgra32;
  x_tiled.pixel_format_modifier = fuchsia_images2::kFormatModifierIntelI915XTiled;

  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(plain, plain));
  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(linear, linear));

  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(plain, linear));
  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(linear, plain));

  EXPECT_FALSE(ImageFormatIsPixelFormatEqual(linear, x_tiled));
  EXPECT_FALSE(ImageFormatIsPixelFormatEqual(plain, x_tiled));
}

// This is fairly redundant with LinearComparison_V2 given that wire enums are the same underlying
// type as natural enums, but we keep this test in case that changes.
TEST(ImageFormat, LinearComparison_V2_wire) {
  fidl::Arena allocator;
  PixelFormatAndModifier plain;
  plain.pixel_format = fuchsia_images2::wire::PixelFormat::kBgra32;

  PixelFormatAndModifier linear;
  linear.pixel_format = fuchsia_images2::PixelFormat::kBgra32;
  linear.pixel_format_modifier = fuchsia_images2::wire::kFormatModifierLinear;

  PixelFormatAndModifier x_tiled;
  x_tiled.pixel_format = fuchsia_images2::PixelFormat::kBgra32;
  x_tiled.pixel_format_modifier = fuchsia_images2::wire::kFormatModifierIntelI915XTiled;

  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(plain, plain));
  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(linear, linear));

  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(plain, linear));
  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(linear, plain));

  EXPECT_FALSE(ImageFormatIsPixelFormatEqual(linear, x_tiled));
  EXPECT_FALSE(ImageFormatIsPixelFormatEqual(plain, x_tiled));
}

TEST(ImageFormat, LinearComparison_V1_wire) {
  sysmem_v1::wire::PixelFormat plain = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = false,
  };

  sysmem_v1::wire::PixelFormat linear = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierLinear,
          },
  };

  sysmem_v1::wire::PixelFormat x_tiled = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierIntelI915XTiled,
          },
  };

  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(plain, plain));
  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(linear, linear));

  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(plain, linear));
  EXPECT_TRUE(ImageFormatIsPixelFormatEqual(linear, plain));

  EXPECT_FALSE(ImageFormatIsPixelFormatEqual(linear, x_tiled));
  EXPECT_FALSE(ImageFormatIsPixelFormatEqual(plain, x_tiled));
}

TEST(ImageFormat, LinearRowBytes_V2) {
  sysmem_v2::ImageFormatConstraints constraints;
  constraints.pixel_format() = fuchsia_images2::PixelFormat::kBgra32;
  constraints.pixel_format_modifier() = fuchsia_images2::kFormatModifierLinear;
  constraints.min_size() = {12u, 1u};
  constraints.max_size() = {100u, 0xFFFFFFFF};
  constraints.bytes_per_row_divisor().emplace(4u * 8u);
  constraints.max_bytes_per_row().emplace(100000u);

  uint32_t row_bytes;
  EXPECT_TRUE(ImageFormatMinimumRowBytes(constraints, 17, &row_bytes));
  EXPECT_EQ(row_bytes, 4 * 24);

  // too narrow in pixels
  EXPECT_FALSE(ImageFormatMinimumRowBytes(constraints, 11, &row_bytes));
  // too wide in pixels
  EXPECT_FALSE(ImageFormatMinimumRowBytes(constraints, 101, &row_bytes));
}

TEST(ImageFormat, LinearRowBytes_V2_wire) {
  fidl::Arena allocator;
  sysmem_v2::wire::ImageFormatConstraints constraints(allocator);
  constraints.set_pixel_format(fuchsia_images2::wire::PixelFormat::kBgra32);
  constraints.set_pixel_format_modifier(allocator, fuchsia_images2::wire::kFormatModifierLinear);
  constraints.set_min_size(allocator, fuchsia_math::wire::SizeU{12u, 1u});
  constraints.set_max_size(allocator, fuchsia_math::wire::SizeU{100u, 0xFFFFFFFF});
  constraints.set_bytes_per_row_divisor(4u * 8u);
  constraints.set_max_bytes_per_row(100000u);

  uint32_t row_bytes;
  EXPECT_TRUE(ImageFormatMinimumRowBytes(constraints, 17, &row_bytes));
  EXPECT_EQ(row_bytes, 4 * 24);

  // too narrow in pixels
  EXPECT_FALSE(ImageFormatMinimumRowBytes(constraints, 11, &row_bytes));
  // too wide in pixels
  EXPECT_FALSE(ImageFormatMinimumRowBytes(constraints, 101, &row_bytes));
}

TEST(ImageFormat, LinearRowBytes_V1_wire) {
  sysmem_v1::wire::PixelFormat linear = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierLinear,
          },
  };
  sysmem_v1::wire::ImageFormatConstraints constraints = {
      .pixel_format = linear,
      .min_coded_width = 12,
      .max_coded_width = 100,
      .max_bytes_per_row = 100000,
      .bytes_per_row_divisor = 4 * 8,
  };

  uint32_t row_bytes;
  EXPECT_TRUE(ImageFormatMinimumRowBytes(constraints, 17, &row_bytes));
  EXPECT_EQ(row_bytes, 4 * 24);

  EXPECT_FALSE(ImageFormatMinimumRowBytes(constraints, 11, &row_bytes));
  EXPECT_FALSE(ImageFormatMinimumRowBytes(constraints, 101, &row_bytes));
}

TEST(ImageFormat, InvalidColorSpace_V1_wire) {
  fuchsia_sysmem::wire::PixelFormat kPixelFormat = {
      .type = fuchsia_sysmem::PixelFormatType::kRgb565,
      .has_format_modifier = true,
      .format_modifier = {
          .value = fuchsia_sysmem::wire::kFormatModifierLinear,
      }};

  sysmem_v1::wire::ColorSpace color_space{sysmem_v1::wire::ColorSpaceType::kInvalid};
  // Shouldn't crash.
  EXPECT_FALSE(ImageFormatIsSupportedColorSpaceForPixelFormat(color_space, kPixelFormat));
}

TEST(ImageFormat, PassThroughColorSpace_V1_wire) {
  fidl::Arena allocator;
  fuchsia_sysmem::wire::PixelFormat linear_bgra = {
      .type = fuchsia_sysmem::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = fuchsia_sysmem::kFormatModifierLinear,
          },
  };

  sysmem_v1::wire::ColorSpace color_space{sysmem_v1::wire::ColorSpaceType::kPassThrough};
  EXPECT_TRUE(ImageFormatIsSupportedColorSpaceForPixelFormat(color_space, linear_bgra));

  fuchsia_sysmem::wire::PixelFormat linear_nv12 = {
      .type = fuchsia_sysmem::wire::PixelFormatType::kNv12,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = fuchsia_sysmem::kFormatModifierLinear,
          },
  };

  EXPECT_TRUE(ImageFormatIsSupportedColorSpaceForPixelFormat(color_space, linear_nv12));
}

TEST(ImageFormat, ZbiPixelFormatV2) {
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_RGB_565);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kRgb565);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_RGB_332);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kRgb332);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_RGB_2220);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kRgb2220);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_ARGB_8888);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kBgra32);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_RGB_X888);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kBgra32);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_MONO_8);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kL8);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_I420);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kI420);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_NV12);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kNv12);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_RGB_888);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kBgr24);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_ABGR_8888);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kR8G8B8A8);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_BGR_888_X);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kR8G8B8A8);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_ARGB_2_10_10_10);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kA2R10G10B10);
  }
  {
    fpromise::result<fuchsia_images2::wire::PixelFormat> convert_result =
        ImageFormatConvertZbiToSysmemPixelFormat_v2(ZBI_PIXEL_FORMAT_ABGR_2_10_10_10);
    ASSERT_TRUE(convert_result.is_ok());
    EXPECT_EQ(convert_result.value(), fuchsia_images2::wire::PixelFormat::kA2B10G10R10);
  }
}

TEST(ImageFormat, PlaneByteOffset_V2) {
  sysmem_v2::ImageFormatConstraints constraints;
  constraints.pixel_format() = fuchsia_images2::PixelFormat::kBgra32;
  constraints.pixel_format_modifier() = fuchsia_images2::kFormatModifierLinear;
  constraints.min_size() = {12u, 12u};
  constraints.max_size() = {100u, 100u};
  constraints.bytes_per_row_divisor().emplace(4u * 8u);
  constraints.max_bytes_per_row().emplace(100000u);

  auto image_format_result = ImageConstraintsToFormat(constraints, 18, 17);
  EXPECT_TRUE(image_format_result.is_ok());
  auto image_format = image_format_result.take_value();
  // The raw size would be 72 without bytes_per_row_divisor of 32.
  EXPECT_EQ(96u, image_format.bytes_per_row());

  uint64_t byte_offset;
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &byte_offset));
  EXPECT_EQ(0u, byte_offset);
  EXPECT_FALSE(ImageFormatPlaneByteOffset(image_format, 1, &byte_offset));

  // clone via generated code
  auto constraints2 = constraints;
  constraints2.pixel_format() = fuchsia_images2::PixelFormat::kI420;

  constexpr uint32_t kBytesPerRow = 32;
  image_format_result = ImageConstraintsToFormat(constraints2, 18, 20);
  EXPECT_TRUE(image_format_result.is_ok());
  image_format = image_format_result.take_value();
  EXPECT_EQ(kBytesPerRow, image_format.bytes_per_row());
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &byte_offset));
  EXPECT_EQ(0u, byte_offset);
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 1, &byte_offset));
  EXPECT_EQ(kBytesPerRow * 20, byte_offset);
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 2, &byte_offset));
  EXPECT_EQ(kBytesPerRow * 20 + kBytesPerRow / 2 * 20 / 2, byte_offset);
  EXPECT_FALSE(ImageFormatPlaneByteOffset(image_format, 3, &byte_offset));

  uint32_t row_bytes;
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 0, &row_bytes));
  EXPECT_EQ(kBytesPerRow, row_bytes);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 1, &row_bytes));
  EXPECT_EQ(kBytesPerRow / 2, row_bytes);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 2, &row_bytes));
  EXPECT_EQ(kBytesPerRow / 2, row_bytes);
  EXPECT_FALSE(ImageFormatPlaneRowBytes(image_format, 3, &row_bytes));
}

TEST(ImageFormat, PlaneByteOffset_V2_wire) {
  fidl::Arena allocator;
  sysmem_v2::wire::ImageFormatConstraints constraints(allocator);
  constraints.set_pixel_format(fuchsia_images2::wire::PixelFormat::kBgra32);
  constraints.set_pixel_format_modifier(allocator, fuchsia_images2::wire::kFormatModifierLinear);
  constraints.set_min_size(allocator, fuchsia_math::wire::SizeU{12u, 12u});
  constraints.set_max_size(allocator, fuchsia_math::wire::SizeU{100u, 100u});
  constraints.set_bytes_per_row_divisor(4u * 8u);
  constraints.set_max_bytes_per_row(100000u);

  auto image_format_result = ImageConstraintsToFormat(allocator, constraints, 18, 17);
  EXPECT_TRUE(image_format_result.is_ok());
  auto image_format = image_format_result.take_value();
  // The raw size would be 72 without bytes_per_row_divisor of 32.
  EXPECT_EQ(96u, image_format.bytes_per_row());

  uint64_t byte_offset;
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &byte_offset));
  EXPECT_EQ(0u, byte_offset);
  EXPECT_FALSE(ImageFormatPlaneByteOffset(image_format, 1, &byte_offset));

  auto constraints2 = sysmem::V2CloneImageFormatConstraints(allocator, constraints);
  constraints2.pixel_format() = fuchsia_images2::PixelFormat::kI420;

  constexpr uint32_t kBytesPerRow = 32;
  image_format_result = ImageConstraintsToFormat(allocator, constraints2, 18, 20);
  EXPECT_TRUE(image_format_result.is_ok());
  image_format = image_format_result.take_value();
  EXPECT_EQ(kBytesPerRow, image_format.bytes_per_row());
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &byte_offset));
  EXPECT_EQ(0u, byte_offset);
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 1, &byte_offset));
  EXPECT_EQ(kBytesPerRow * 20, byte_offset);
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 2, &byte_offset));
  EXPECT_EQ(kBytesPerRow * 20 + kBytesPerRow / 2 * 20 / 2, byte_offset);
  EXPECT_FALSE(ImageFormatPlaneByteOffset(image_format, 3, &byte_offset));

  uint32_t row_bytes;
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 0, &row_bytes));
  EXPECT_EQ(kBytesPerRow, row_bytes);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 1, &row_bytes));
  EXPECT_EQ(kBytesPerRow / 2, row_bytes);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 2, &row_bytes));
  EXPECT_EQ(kBytesPerRow / 2, row_bytes);
  EXPECT_FALSE(ImageFormatPlaneRowBytes(image_format, 3, &row_bytes));
}

TEST(ImageFormat, PlaneByteOffset_V1_wire) {
  sysmem_v1::wire::PixelFormat linear = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierLinear,
          },
  };
  sysmem_v1::wire::ImageFormatConstraints constraints = {
      .pixel_format = linear,
      .min_coded_width = 12,
      .max_coded_width = 100,
      .min_coded_height = 12,
      .max_coded_height = 100,
      .max_bytes_per_row = 100000,
      .bytes_per_row_divisor = 4 * 8,
  };

  auto image_format_result = ImageConstraintsToFormat(constraints, 18, 17);
  EXPECT_TRUE(image_format_result.is_ok());
  auto image_format = image_format_result.take_value();
  // The raw size would be 72 without bytes_per_row_divisor of 32.
  EXPECT_EQ(96u, image_format.bytes_per_row);

  uint64_t byte_offset;
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &byte_offset));
  EXPECT_EQ(0u, byte_offset);
  EXPECT_FALSE(ImageFormatPlaneByteOffset(image_format, 1, &byte_offset));

  constraints.pixel_format.type = sysmem_v1::wire::PixelFormatType::kI420;

  constexpr uint32_t kBytesPerRow = 32;
  image_format_result = ImageConstraintsToFormat(constraints, 18, 20);
  EXPECT_TRUE(image_format_result.is_ok());
  image_format = image_format_result.take_value();
  EXPECT_EQ(kBytesPerRow, image_format.bytes_per_row);
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &byte_offset));
  EXPECT_EQ(0u, byte_offset);
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 1, &byte_offset));
  EXPECT_EQ(kBytesPerRow * 20, byte_offset);
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 2, &byte_offset));
  EXPECT_EQ(kBytesPerRow * 20 + kBytesPerRow / 2 * 20 / 2, byte_offset);
  EXPECT_FALSE(ImageFormatPlaneByteOffset(image_format, 3, &byte_offset));

  uint32_t row_bytes;
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 0, &row_bytes));
  EXPECT_EQ(kBytesPerRow, row_bytes);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 1, &row_bytes));
  EXPECT_EQ(kBytesPerRow / 2, row_bytes);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 2, &row_bytes));
  EXPECT_EQ(kBytesPerRow / 2, row_bytes);
  EXPECT_FALSE(ImageFormatPlaneRowBytes(image_format, 3, &row_bytes));
}

TEST(ImageFormat, TransactionEliminationFormats_V2) {
  PixelFormatAndModifier format;
  format.pixel_format = fuchsia_images2::PixelFormat::kBgra32;
  format.pixel_format_modifier = fuchsia_images2::kFormatModifierLinear;

  EXPECT_TRUE(ImageFormatCompatibleWithProtectedMemory(format));

  auto format2 = format;
  format2.pixel_format_modifier = fuchsia_images2::kFormatModifierArmLinearTe;

  EXPECT_FALSE(ImageFormatCompatibleWithProtectedMemory(format2));

  sysmem_v2::ImageFormatConstraints constraints;
  constraints.pixel_format() = format2.pixel_format;
  constraints.pixel_format_modifier() = format2.pixel_format_modifier;
  constraints.min_size() = {12u, 12u};
  constraints.max_size() = {100u, 100u};
  constraints.bytes_per_row_divisor().emplace(4u * 8u);
  constraints.max_bytes_per_row().emplace(100000u);

  auto image_format_result = ImageConstraintsToFormat(constraints, 18, 17);
  EXPECT_TRUE(image_format_result.is_ok());
  auto image_format = image_format_result.take_value();
  // The raw size would be 72 without bytes_per_row_divisor of 32.
  EXPECT_EQ(96u, image_format.bytes_per_row());

  // Check the color plane data.
  uint32_t row_bytes;
  uint64_t plane_offset;
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &plane_offset));
  EXPECT_EQ(0u, plane_offset);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 0, &row_bytes));
  EXPECT_EQ(image_format.bytes_per_row(), row_bytes);

  constexpr uint32_t kTePlane = 3;
  // Check the TE plane data.
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, kTePlane, &plane_offset));
  EXPECT_LE(image_format.bytes_per_row().value() * 17, plane_offset);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, kTePlane, &row_bytes));

  // Row size should be rounded up to 64 bytes.
  EXPECT_EQ(64, row_bytes);
}

TEST(ImageFormat, TransactionEliminationFormats_V2_wire) {
  fidl::Arena allocator;
  PixelFormatAndModifier format;
  format.pixel_format = fuchsia_images2::wire::PixelFormat::kBgra32;
  format.pixel_format_modifier = fuchsia_images2::wire::kFormatModifierLinear;

  EXPECT_TRUE(ImageFormatCompatibleWithProtectedMemory(format));

  auto format2 = format;
  format2.pixel_format_modifier = fuchsia_images2::wire::kFormatModifierArmLinearTe;

  EXPECT_FALSE(ImageFormatCompatibleWithProtectedMemory(format2));

  sysmem_v2::wire::ImageFormatConstraints constraints(allocator);
  constraints.set_pixel_format(format2.pixel_format);
  constraints.set_pixel_format_modifier(allocator, format2.pixel_format_modifier);
  constraints.set_min_size(allocator, fuchsia_math::wire::SizeU{12u, 12u});
  constraints.set_max_size(allocator, fuchsia_math::wire::SizeU{100u, 100u});
  constraints.set_bytes_per_row_divisor(4u * 8u);
  constraints.set_max_bytes_per_row(100000u);

  auto image_format_result = ImageConstraintsToFormat(allocator, constraints, 18, 17);
  EXPECT_TRUE(image_format_result.is_ok());
  auto image_format = image_format_result.take_value();
  // The raw size would be 72 without bytes_per_row_divisor of 32.
  EXPECT_EQ(96u, image_format.bytes_per_row());

  // Check the color plane data.
  uint32_t row_bytes;
  uint64_t plane_offset;
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &plane_offset));
  EXPECT_EQ(0u, plane_offset);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 0, &row_bytes));
  EXPECT_EQ(image_format.bytes_per_row(), row_bytes);

  constexpr uint32_t kTePlane = 3;
  // Check the TE plane data.
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, kTePlane, &plane_offset));
  EXPECT_LE(image_format.bytes_per_row() * 17, plane_offset);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, kTePlane, &row_bytes));

  // Row size should be rounded up to 64 bytes.
  EXPECT_EQ(64, row_bytes);
}

TEST(ImageFormat, TransactionEliminationFormats_V1_wire) {
  sysmem_v1::wire::PixelFormat format = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierLinear,
          },
  };
  EXPECT_TRUE(ImageFormatCompatibleWithProtectedMemory(format));

  format.format_modifier.value = sysmem_v1::wire::kFormatModifierArmLinearTe;
  EXPECT_FALSE(ImageFormatCompatibleWithProtectedMemory(format));

  sysmem_v1::wire::ImageFormatConstraints constraints = {
      .pixel_format = format,
      .min_coded_width = 12,
      .max_coded_width = 100,
      .min_coded_height = 12,
      .max_coded_height = 100,
      .max_bytes_per_row = 100000,
      .bytes_per_row_divisor = 4 * 8,
  };

  auto optional_format = ImageConstraintsToFormat(constraints, 18, 17);
  EXPECT_TRUE(optional_format);
  auto& image_format = optional_format.value();
  // The raw size would be 72 without bytes_per_row_divisor of 32.
  EXPECT_EQ(96u, image_format.bytes_per_row);

  // Check the color plane data.
  uint32_t row_bytes;
  uint64_t plane_offset;
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, 0, &plane_offset));
  EXPECT_EQ(0u, plane_offset);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, 0, &row_bytes));
  EXPECT_EQ(image_format.bytes_per_row, row_bytes);

  constexpr uint32_t kTePlane = 3;
  // Check the TE plane data.
  EXPECT_TRUE(ImageFormatPlaneByteOffset(image_format, kTePlane, &plane_offset));
  EXPECT_LE(image_format.bytes_per_row * 17, plane_offset);
  EXPECT_TRUE(ImageFormatPlaneRowBytes(image_format, kTePlane, &row_bytes));

  // Row size should be rounded up to 64 bytes.
  EXPECT_EQ(64, row_bytes);
}

TEST(ImageFormat, BasicSizes_V2) {
  constexpr uint32_t kWidth = 64;
  constexpr uint32_t kHeight = 128;
  constexpr uint32_t kStride = kWidth * 6;

  fuchsia_images2::ImageFormat image_format_bgra32;
  image_format_bgra32.pixel_format() = fuchsia_images2::PixelFormat::kBgra32;
  image_format_bgra32.size() = {kWidth, kHeight};
  image_format_bgra32.bytes_per_row().emplace(kStride);
  EXPECT_EQ(kHeight * kStride, ImageFormatImageSize(image_format_bgra32));
  auto pixel_format_and_modifier_bgra32 =
      PixelFormatAndModifierFromImageFormat(image_format_bgra32);
  EXPECT_EQ(1, ImageFormatSurfaceWidthMinDivisor(pixel_format_and_modifier_bgra32));
  EXPECT_EQ(1, ImageFormatSurfaceHeightMinDivisor(pixel_format_and_modifier_bgra32));
  EXPECT_EQ(4, ImageFormatSampleAlignment(pixel_format_and_modifier_bgra32));

  fuchsia_images2::ImageFormat image_format_nv12;
  image_format_nv12.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_format_nv12.size() = {kWidth, kHeight};
  image_format_nv12.bytes_per_row() = kStride;
  EXPECT_EQ(kHeight * kStride * 3 / 2, ImageFormatImageSize(image_format_nv12));
  auto pixel_format_and_modifier_nv12 = PixelFormatAndModifierFromImageFormat(image_format_nv12);
  EXPECT_EQ(2, ImageFormatSurfaceWidthMinDivisor(pixel_format_and_modifier_nv12));
  EXPECT_EQ(2, ImageFormatSurfaceHeightMinDivisor(pixel_format_and_modifier_nv12));
  EXPECT_EQ(2, ImageFormatSampleAlignment(pixel_format_and_modifier_nv12));
}

TEST(ImageFormat, BasicSizes_V2_wire) {
  fidl::Arena allocator;
  constexpr uint32_t kWidth = 64;
  constexpr uint32_t kHeight = 128;
  constexpr uint32_t kStride = kWidth * 6;

  fuchsia_images2::wire::ImageFormat image_format_bgra32(allocator);
  image_format_bgra32.set_pixel_format(fuchsia_images2::PixelFormat::kBgra32);
  image_format_bgra32.set_size(allocator, fuchsia_math::wire::SizeU{kWidth, kHeight});
  image_format_bgra32.set_bytes_per_row(kStride);
  EXPECT_EQ(kHeight * kStride, ImageFormatImageSize(image_format_bgra32));
  auto pixel_format_and_modifier_bgra32 =
      PixelFormatAndModifierFromImageFormat(image_format_bgra32);
  EXPECT_EQ(1, ImageFormatSurfaceWidthMinDivisor(pixel_format_and_modifier_bgra32));
  EXPECT_EQ(1, ImageFormatSurfaceHeightMinDivisor(pixel_format_and_modifier_bgra32));
  EXPECT_EQ(4, ImageFormatSampleAlignment(pixel_format_and_modifier_bgra32));

  fuchsia_images2::wire::ImageFormat image_format_nv12(allocator);
  image_format_nv12.set_pixel_format(fuchsia_images2::PixelFormat::kNv12);
  image_format_nv12.set_size(allocator, fuchsia_math::wire::SizeU{kWidth, kHeight});
  image_format_nv12.set_bytes_per_row(kStride);
  EXPECT_EQ(kHeight * kStride * 3 / 2, ImageFormatImageSize(image_format_nv12));
  auto pixel_format_and_modifier_nv12 = PixelFormatAndModifierFromImageFormat(image_format_nv12);
  EXPECT_EQ(2, ImageFormatSurfaceWidthMinDivisor(pixel_format_and_modifier_nv12));
  EXPECT_EQ(2, ImageFormatSurfaceHeightMinDivisor(pixel_format_and_modifier_nv12));
  EXPECT_EQ(2, ImageFormatSampleAlignment(pixel_format_and_modifier_nv12));
}

TEST(ImageFormat, BasicSizes_V1_wire) {
  constexpr uint32_t kWidth = 64;
  constexpr uint32_t kHeight = 128;
  constexpr uint32_t kStride = 256;

  sysmem_v1::wire::ImageFormat2 image_format_bgra32 = {
      .pixel_format =
          {
              .type = sysmem_v1::wire::PixelFormatType::kBgra32,
          },
      .coded_width = kWidth,
      .coded_height = kHeight,
      .bytes_per_row = kStride,
  };
  EXPECT_EQ(kHeight * kStride, ImageFormatImageSize(image_format_bgra32));
  EXPECT_EQ(1, ImageFormatCodedWidthMinDivisor(image_format_bgra32.pixel_format));
  EXPECT_EQ(1, ImageFormatCodedHeightMinDivisor(image_format_bgra32.pixel_format));
  EXPECT_EQ(4, ImageFormatSampleAlignment(image_format_bgra32.pixel_format));

  sysmem_v1::wire::ImageFormat2 image_format_nv12 = {
      .pixel_format =
          {
              .type = sysmem_v1::wire::PixelFormatType::kNv12,
          },
      .coded_width = kWidth,
      .coded_height = kHeight,
      .bytes_per_row = kStride,
  };
  EXPECT_EQ(kHeight * kStride * 3 / 2, ImageFormatImageSize(image_format_nv12));
  EXPECT_EQ(2, ImageFormatCodedWidthMinDivisor(image_format_nv12.pixel_format));
  EXPECT_EQ(2, ImageFormatCodedHeightMinDivisor(image_format_nv12.pixel_format));
  EXPECT_EQ(2, ImageFormatSampleAlignment(image_format_nv12.pixel_format));
}

TEST(ImageFormat, AfbcFlagFormats_V1_wire) {
  sysmem_v1::wire::PixelFormat format = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTe,
          },
  };

  EXPECT_FALSE(ImageFormatCompatibleWithProtectedMemory(format));

  sysmem_v1::wire::ImageFormatConstraints constraints = {
      .pixel_format = format,
      .min_coded_width = 12,
      .max_coded_width = 100,
      .min_coded_height = 12,
      .max_coded_height = 100,
      .max_bytes_per_row = 100000,
      .bytes_per_row_divisor = 4 * 8,
  };

  auto optional_format = ImageConstraintsToFormat(constraints, 18, 17);
  EXPECT_TRUE(optional_format);

  sysmem_v1::wire::PixelFormat tiled_format = {
      .type = sysmem_v1::wire::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTiledHeader,
          },
  };

  constraints.pixel_format = tiled_format;

  optional_format = ImageConstraintsToFormat(constraints, 18, 17);
  EXPECT_TRUE(optional_format);
  auto& image_format = optional_format.value();
  constexpr uint32_t kMinHeaderOffset = 4096u;
  constexpr uint32_t kMinWidth = 128;
  constexpr uint32_t kMinHeight = 128;
  EXPECT_EQ(kMinHeaderOffset + kMinWidth * kMinHeight * 4, ImageFormatImageSize(image_format));
}

TEST(ImageFormat, R8G8Formats_V1_wire) {
  sysmem_v1::wire::PixelFormat format = {
      .type = sysmem_v1::wire::PixelFormatType::kR8G8,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = sysmem_v1::wire::kFormatModifierLinear,
          },
  };

  sysmem_v1::wire::ImageFormatConstraints constraints = {
      .pixel_format = format,
      .min_coded_width = 12,
      .max_coded_width = 100,
      .min_coded_height = 12,
      .max_coded_height = 100,
      .max_bytes_per_row = 100000,
      .bytes_per_row_divisor = 1,
  };

  {
    auto optional_format = ImageConstraintsToFormat(constraints, 18, 17);
    EXPECT_TRUE(optional_format);
    auto& image_format = optional_format.value();
    EXPECT_EQ(18u * 2, image_format.bytes_per_row);
    EXPECT_EQ(18u * 17u * 2, ImageFormatImageSize(image_format));
  }

  constraints.pixel_format.type = sysmem_v1::wire::PixelFormatType::kR8;

  {
    auto optional_format = ImageConstraintsToFormat(constraints, 18, 17);
    EXPECT_TRUE(optional_format);
    auto& image_format = optional_format.value();
    EXPECT_EQ(18u * 1, image_format.bytes_per_row);
    EXPECT_EQ(18u * 17u * 1, ImageFormatImageSize(image_format));
  }
}

TEST(ImageFormat, A2R10G10B10_Formats_V1_wire) {
  for (const auto& pixel_format_type : {sysmem_v1::wire::PixelFormatType::kA2R10G10B10,
                                        sysmem_v1::wire::PixelFormatType::kA2B10G10R10}) {
    sysmem_v1::wire::PixelFormat format = {
        .type = pixel_format_type,
        .has_format_modifier = true,
        .format_modifier =
            {
                .value = sysmem_v1::wire::kFormatModifierLinear,
            },
    };

    sysmem_v1::wire::ImageFormatConstraints constraints = {
        .pixel_format = format,
        .min_coded_width = 12,
        .max_coded_width = 100,
        .min_coded_height = 12,
        .max_coded_height = 100,
        .max_bytes_per_row = 100000,
        .bytes_per_row_divisor = 1,
    };

    auto optional_format = ImageConstraintsToFormat(constraints, 18, 17);
    EXPECT_TRUE(optional_format);
    auto& image_format = optional_format.value();
    EXPECT_EQ(18u * 4, image_format.bytes_per_row);
    EXPECT_EQ(18u * 17u * 4, ImageFormatImageSize(image_format));
    EXPECT_EQ(1, ImageFormatCodedWidthMinDivisor(image_format.pixel_format));
    EXPECT_EQ(1, ImageFormatCodedHeightMinDivisor(image_format.pixel_format));
    EXPECT_EQ(4, ImageFormatSampleAlignment(image_format.pixel_format));
  }
}

TEST(ImageFormat, GoldfishOptimal_V2) {
  constexpr uint32_t kWidth = 64;
  constexpr uint32_t kHeight = 128;
  constexpr uint32_t kStride = kWidth * 6;

  fuchsia_images2::ImageFormat linear_image_format_bgra32;
  linear_image_format_bgra32.pixel_format() = fuchsia_images2::PixelFormat::kBgra32;
  linear_image_format_bgra32.size() = {kWidth, kHeight};
  linear_image_format_bgra32.bytes_per_row().emplace(kStride);

  fuchsia_images2::ImageFormat goldfish_optimal_image_format_bgra32;
  goldfish_optimal_image_format_bgra32.pixel_format() = fuchsia_images2::PixelFormat::kBgra32;
  goldfish_optimal_image_format_bgra32.pixel_format_modifier() =
      fuchsia_images2::kFormatModifierGoogleGoldfishOptimal;
  goldfish_optimal_image_format_bgra32.size() = {kWidth, kHeight};
  goldfish_optimal_image_format_bgra32.bytes_per_row().emplace(kStride);
  EXPECT_EQ(ImageFormatImageSize(linear_image_format_bgra32),
            ImageFormatImageSize(goldfish_optimal_image_format_bgra32));
  auto pixel_format_and_modifier_linear_bgra32 =
      PixelFormatAndModifierFromImageFormat(linear_image_format_bgra32);
  auto pixel_format_and_modifier_goldfish_bgra32 =
      PixelFormatAndModifierFromImageFormat(goldfish_optimal_image_format_bgra32);
  EXPECT_EQ(ImageFormatSurfaceWidthMinDivisor(pixel_format_and_modifier_linear_bgra32),
            ImageFormatSurfaceWidthMinDivisor(pixel_format_and_modifier_goldfish_bgra32));
  EXPECT_EQ(ImageFormatSurfaceHeightMinDivisor(pixel_format_and_modifier_linear_bgra32),
            ImageFormatSurfaceHeightMinDivisor(pixel_format_and_modifier_goldfish_bgra32));
  EXPECT_EQ(ImageFormatSampleAlignment(pixel_format_and_modifier_linear_bgra32),
            ImageFormatSampleAlignment(pixel_format_and_modifier_goldfish_bgra32));
}

TEST(ImageFormat, GoldfishOptimal_V2_wire) {
  fidl::Arena allocator;
  constexpr uint32_t kWidth = 64;
  constexpr uint32_t kHeight = 128;
  constexpr uint32_t kStride = kWidth * 6;

  fuchsia_images2::wire::ImageFormat linear_image_format_bgra32(allocator);
  linear_image_format_bgra32.set_pixel_format(fuchsia_images2::wire::PixelFormat::kBgra32);
  linear_image_format_bgra32.set_size(allocator, fuchsia_math::wire::SizeU{kWidth, kHeight});
  linear_image_format_bgra32.set_bytes_per_row(kStride);

  fuchsia_images2::wire::ImageFormat goldfish_optimal_image_format_bgra32(allocator);
  goldfish_optimal_image_format_bgra32.set_pixel_format(
      fuchsia_images2::wire::PixelFormat::kBgra32);
  goldfish_optimal_image_format_bgra32.set_pixel_format_modifier(
      allocator, fuchsia_images2::wire::kFormatModifierGoogleGoldfishOptimal);
  goldfish_optimal_image_format_bgra32.set_size(allocator,
                                                fuchsia_math::wire::SizeU{kWidth, kHeight});
  goldfish_optimal_image_format_bgra32.set_bytes_per_row(kStride);
  EXPECT_EQ(ImageFormatImageSize(linear_image_format_bgra32),
            ImageFormatImageSize(goldfish_optimal_image_format_bgra32));
  auto linear_pixel_format_and_modifier =
      PixelFormatAndModifierFromImageFormat(linear_image_format_bgra32);
  auto goldfish_optimal_pixel_format_and_modifier =
      PixelFormatAndModifierFromImageFormat(goldfish_optimal_image_format_bgra32);
  EXPECT_EQ(ImageFormatSurfaceWidthMinDivisor(linear_pixel_format_and_modifier),
            ImageFormatSurfaceWidthMinDivisor(goldfish_optimal_pixel_format_and_modifier));
  EXPECT_EQ(ImageFormatSurfaceHeightMinDivisor(linear_pixel_format_and_modifier),
            ImageFormatSurfaceHeightMinDivisor(goldfish_optimal_pixel_format_and_modifier));
  EXPECT_EQ(ImageFormatSampleAlignment(linear_pixel_format_and_modifier),
            ImageFormatSampleAlignment(goldfish_optimal_pixel_format_and_modifier));
}

TEST(ImageFormat, CorrectModifiers) {
  EXPECT_EQ(sysmem_v1::kFormatModifierArmAfbc16X16YuvTiledHeader,
            sysmem_v1::kFormatModifierArmAfbc16X16YuvTiledHeader);
  EXPECT_EQ(sysmem_v1::kFormatModifierArmAfbc16X16YuvTiledHeader,
            sysmem_v1::kFormatModifierArmAfbc16X16 | sysmem_v1::kFormatModifierArmYuvBit |
                sysmem_v1::kFormatModifierArmTiledHeaderBit);
  // V2 changes the value to be less likely to collide.
  EXPECT_NE(sysmem_v1::kFormatModifierGoogleGoldfishOptimal,
            fuchsia_images2::kFormatModifierGoogleGoldfishOptimal);
}

TEST(ImageFormat, CorrectModifiers_wire) {
  EXPECT_EQ(sysmem_v1::wire::kFormatModifierArmAfbc16X16YuvTiledHeader,
            sysmem_v1::wire::kFormatModifierArmAfbc16X16YuvTiledHeader);
  EXPECT_EQ(sysmem_v1::wire::kFormatModifierArmAfbc16X16YuvTiledHeader,
            sysmem_v1::wire::kFormatModifierArmAfbc16X16 |
                sysmem_v1::wire::kFormatModifierArmYuvBit |
                sysmem_v1::wire::kFormatModifierArmTiledHeaderBit);
  // V2 changes the value to be less likely to collide.
  EXPECT_NE(sysmem_v1::wire::kFormatModifierGoogleGoldfishOptimal,
            fuchsia_images2::wire::kFormatModifierGoogleGoldfishOptimal);
}

TEST(ImageFormat, RoundUpWidthForCallers) {
  sysmem_v2::ImageFormatConstraints constraints;
  constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  constraints.pixel_format_modifier() = fuchsia_images2::kFormatModifierLinear;
  constraints.min_size() = {12u, 1u};
  constraints.max_size() = {100u, 0xFFFFFFFF};

  // Later we specify a width that isn't already aligned to size_alignment.width.
  constraints.size_alignment() = {8, 1};

  // Ensure that ImageFormatMinimumRowBytes rounds up for the caller.
  uint32_t minimum_row_bytes;
  bool result = ImageFormatMinimumRowBytes(constraints, 17, &minimum_row_bytes);
  ASSERT_TRUE(result);
  // Lowest value that's 17 or larger and divisible by 8.
  EXPECT_EQ(24, minimum_row_bytes);
}
