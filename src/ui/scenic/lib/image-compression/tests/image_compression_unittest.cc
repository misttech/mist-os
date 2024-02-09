// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/image-compression/image_compression.h"

#include <fidl/fuchsia.ui.compression.internal/cpp/hlcpp_conversion.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/macros.h>
#include <png.h>

#include <iostream>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

using image_compression::ImageCompression;

namespace {

// Needed for |png_set_read_fn| so that libpng can read from a zx::vmo in a stream-like fashion.
struct libpng_vmo {
  zx::vmo* vmo;
  size_t offset;
};

constexpr uint8_t kPNGHeaderBytes = 8;
constexpr size_t kBytesPerPixel = 4;

// Converts from a PNG VMO into a BGRA byte vector.
std::vector<uint8_t> PngToBGRA(fuchsia::math::SizeU size, zx::vmo& png_vmo) {
  // Check if |png_vmo| is actually a PNG.
  unsigned char header[kPNGHeaderBytes];
  png_vmo.read(&header[0], 0, kPNGHeaderBytes);
  FX_DCHECK(png_sig_cmp(header, 0, kPNGHeaderBytes) == 0) << "tried to convert non-PNG VMO";

  png_structp png = png_create_read_struct(PNG_LIBPNG_VER_STRING, nullptr, nullptr, nullptr);
  FX_DCHECK(png) << "png_create_read_struct failed";

  png_infop info = png_create_info_struct(png);
  FX_DCHECK(info) << "png_create_info_struct failed";

  // Tell libpng how to read from a zx::vmo in a stream-like fashion.
  libpng_vmo read_fn_vmo = {.vmo = &png_vmo, .offset = 0u};
  png_set_read_fn(png, &read_fn_vmo,
                  [](png_structp png_ptr, png_bytep out_bytes, size_t length) -> void {
                    // Read |length| bytes into |out_bytes| from the VMO.
                    libpng_vmo* vmo = reinterpret_cast<libpng_vmo*>(png_get_io_ptr(png_ptr));
                    vmo->vmo->read(out_bytes, vmo->offset, length);
                    vmo->offset += length;
                  });

  png_read_info(png, info);

  // Sanity check width and height are as expected.
  uint32_t width = png_get_image_width(png, info);
  uint32_t height = png_get_image_height(png, info);
  FX_DCHECK(width == size.width);
  FX_DCHECK(height == size.height);

  uint32_t color_type = png_get_color_type(png, info);
  uint32_t bit_depth = png_get_bit_depth(png, info);

  // Only works with 4 bytes (32-bits) per pixel.
  FX_CHECK(color_type == PNG_COLOR_TYPE_RGBA) << "currently only supports RGBA";
  FX_CHECK(bit_depth == 8) << "currently only supports 8-bit channel";

  int64_t row_bytes = png_get_rowbytes(png, info);
  int64_t expected_row_bytes = kBytesPerPixel * width;  // We assume each pixel is 4 bytes.
  FX_DCHECK(row_bytes == expected_row_bytes)
      << "unexpected row_bytes: " << row_bytes << " expect: 4 * " << width;

  const uint64_t bytesPerRow = png_get_rowbytes(png, info);
  uint8_t* rowData = new uint8_t[bytesPerRow];

  std::vector<uint8_t> output;

  // Read one row at a time. For some reason, this is necessary instead of |png_read_image()|. Maybe
  // because we're reading from memory instead of a file?
  for (uint32_t rowIdx = 0; rowIdx < height; ++rowIdx) {
    png_read_row(png, static_cast<png_bytep>(rowData), nullptr);

    uint32_t byteIndex = 0;
    for (uint32_t colIdx = 0; colIdx < width; ++colIdx) {
      uint8_t red = rowData[byteIndex++];
      uint8_t green = rowData[byteIndex++];
      uint8_t blue = rowData[byteIndex++];
      uint8_t alpha = rowData[byteIndex++];

      output.push_back(blue);
      output.push_back(green);
      output.push_back(red);
      output.push_back(alpha);
    }
  }

  png_destroy_read_struct(&png, &info, nullptr);

  return output;
}

}  // namespace

class ImageCompressionTest : public gtest::TestLoopFixture {
 protected:
  void SetUp() override {
    image_compression_ = std::make_unique<ImageCompression>();
    image_compression_->Connect(fidl::HLCPPToNatural(client_ptr_.NewRequest()), dispatcher());

    // Handle 4k tests.
    if (is_4k_) {
      size_.width = 3840;
      size_.height = 2160;
      bytes_to_write_ = size_.width * size_.height * kBytesPerPixel;
    }

    // Define in_vmo.
    zx_status_t status = zx::vmo::create(bytes_to_write_, 0, &in_vmo_);
    EXPECT_EQ(status, ZX_OK);

    // Define out_vmo.
    status =
        zx::vmo::create(bytes_to_write_ + zx_system_get_page_size(), ZX_VMO_RESIZABLE, &out_vmo_);
    EXPECT_EQ(status, ZX_OK);

    // Duplicate in_ and out_ VMO.
    EXPECT_EQ(in_vmo_.duplicate(ZX_RIGHT_SAME_RIGHTS, &in_vmo_copy_), ZX_OK);
    EXPECT_EQ(out_vmo_.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_vmo_copy_), ZX_OK);
  }

  fuchsia::ui::compression::internal::ImageCompressorPtr client_ptr_;
  std::unique_ptr<ImageCompression> image_compression_;

  // By default, assume 1080p BGRA sized buffers.
  fuchsia::math::SizeU size_ = {.width = 1920, .height = 1080};
  size_t bytes_to_write_ = size_.width * size_.height * kBytesPerPixel;

  zx::vmo in_vmo_;
  zx::vmo out_vmo_;

  // Copies of VMO to pass into function.
  zx::vmo in_vmo_copy_;
  zx::vmo out_vmo_copy_;

  bool is_4k_ = false;
};

class ParameterizedImageCompressionTest : public ImageCompressionTest,
                                          public ::testing::WithParamInterface<bool> {
 protected:
  void SetUp() override {
    is_4k_ = GetParam();
    ImageCompressionTest::SetUp();
  }
};

INSTANTIATE_TEST_SUITE_P(UseFlatland, ParameterizedImageCompressionTest, ::testing::Bool());

TEST_P(ParameterizedImageCompressionTest, ValidImage) {
  size_t num_pixels = size_.width * size_.height;

  std::vector<uint8_t> pixels;
  for (size_t i = 0; i < num_pixels; ++i) {
    uint8_t inc = static_cast<uint8_t>(i);
    pixels.push_back(15 + inc);
    pixels.push_back(112 + inc);
    pixels.push_back(122 + inc);
    pixels.push_back(251 + inc);
  }

  EXPECT_EQ(in_vmo_.write(pixels.data(), 0, bytes_to_write_), ZX_OK);

  fuchsia::ui::compression::internal::ImageCompressorEncodePngRequest request;
  request.set_raw_vmo(std::move(in_vmo_copy_));
  request.set_image_dimensions(size_);
  request.set_png_vmo(std::move(out_vmo_copy_));

  bool flag = false;
  client_ptr_->EncodePng(
      std::move(request),
      [&flag](fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result) {
        EXPECT_TRUE(result.is_response());

        flag = true;
      });

  RunLoopUntilIdle();
  EXPECT_EQ(flag, true);

  // Compare the original |pixels| and |reconverted_vmo|.
  std::vector<uint8_t> reconverted_vmo = PngToBGRA(size_, out_vmo_);
  EXPECT_EQ(reconverted_vmo.size(), pixels.size());

  for (size_t i = 0; i < reconverted_vmo.size(); ++i) {
    EXPECT_EQ(reconverted_vmo[i], pixels[i]);
  }

  // Expect some compression to have occurred.
  size_t after_size;
  out_vmo_.get_size(&after_size);
  EXPECT_LT(after_size, bytes_to_write_ + zx_system_get_page_size());
}

TEST_F(ImageCompressionTest, MissingArgs) {
  // Only include in vmo.
  fuchsia::ui::compression::internal::ImageCompressorEncodePngRequest request;
  request.set_raw_vmo(std::move(in_vmo_copy_));

  bool flag = false;
  client_ptr_->EncodePng(
      std::move(request),
      [&flag](fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result) {
        EXPECT_TRUE(result.is_err());
        EXPECT_EQ(result.err(),
                  fuchsia::ui::compression::internal::ImageCompressionError::MISSING_ARGS);

        flag = true;
      });

  RunLoopUntilIdle();
  EXPECT_EQ(flag, true);

  // Only include size.
  request = {};
  request.set_image_dimensions(size_);
  flag = false;

  client_ptr_->EncodePng(
      std::move(request),
      [&flag](fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result) {
        EXPECT_TRUE(result.is_err());
        EXPECT_EQ(result.err(),
                  fuchsia::ui::compression::internal::ImageCompressionError::MISSING_ARGS);

        flag = true;
      });

  RunLoopUntilIdle();
  EXPECT_EQ(flag, true);

  // Only include out vmo.
  request = {};
  request.set_png_vmo(std::move(out_vmo_copy_));
  flag = false;

  client_ptr_->EncodePng(
      std::move(request),
      [&flag](fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result) {
        EXPECT_TRUE(result.is_err());
        EXPECT_EQ(result.err(),
                  fuchsia::ui::compression::internal::ImageCompressionError::MISSING_ARGS);

        flag = true;
      });

  RunLoopUntilIdle();
  EXPECT_EQ(flag, true);
}

TEST_F(ImageCompressionTest, EmptyInVmo) {
  // Define empty in_vmo.
  zx::vmo in_vmo;
  zx_status_t status = zx::vmo::create(0, 0, &in_vmo);
  EXPECT_EQ(status, ZX_OK);

  fuchsia::ui::compression::internal::ImageCompressorEncodePngRequest request;
  request.set_raw_vmo(std::move(in_vmo));
  request.set_image_dimensions(size_);
  request.set_png_vmo(std::move(out_vmo_copy_));

  bool flag = false;
  client_ptr_->EncodePng(
      std::move(request),
      [&flag](fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result) {
        EXPECT_TRUE(result.is_err());
        EXPECT_EQ(result.err(),
                  fuchsia::ui::compression::internal::ImageCompressionError::INVALID_ARGS);

        flag = true;
      });

  RunLoopUntilIdle();
  EXPECT_EQ(flag, true);
}

TEST_F(ImageCompressionTest, OutVmoTooSmall) {
  // Define small out_vmo, by 1 page size.
  zx::vmo small_out_vmo;
  zx_status_t status = zx::vmo::create(bytes_to_write_, 0, &small_out_vmo);
  EXPECT_EQ(status, ZX_OK);

  fuchsia::ui::compression::internal::ImageCompressorEncodePngRequest request;
  request.set_raw_vmo(std::move(in_vmo_copy_));
  request.set_image_dimensions(size_);
  request.set_png_vmo(std::move(small_out_vmo));

  bool flag = false;
  client_ptr_->EncodePng(
      std::move(request),
      [&flag](fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result) {
        EXPECT_TRUE(result.is_err());
        EXPECT_EQ(result.err(),
                  fuchsia::ui::compression::internal::ImageCompressionError::INVALID_ARGS);

        flag = true;
      });

  RunLoopUntilIdle();
  EXPECT_EQ(flag, true);
}

// Try to compress a BGRA image with a stated width and height that is larger than the VMO size.
TEST_F(ImageCompressionTest, VmoSizeIncompatibleWithWidthAndHeight) {
  // Define small in_vmo, by 1 page size.
  zx::vmo small_in_vmo;
  zx_status_t status =
      zx::vmo::create(bytes_to_write_ - zx_system_get_page_size(), 0, &small_in_vmo);
  EXPECT_EQ(status, ZX_OK);

  fuchsia::ui::compression::internal::ImageCompressorEncodePngRequest request;
  request.set_raw_vmo(std::move(small_in_vmo));
  request.set_image_dimensions(size_);
  request.set_png_vmo(std::move(out_vmo_copy_));

  bool flag = false;
  client_ptr_->EncodePng(
      std::move(request),
      [&flag](fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result) {
        EXPECT_TRUE(result.is_err());
        EXPECT_EQ(result.err(),
                  fuchsia::ui::compression::internal::ImageCompressionError::INVALID_ARGS);

        flag = true;
      });

  RunLoopUntilIdle();
  EXPECT_EQ(flag, true);
}
