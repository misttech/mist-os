// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/display-info.h"

#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/edid-values/edid-values.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

namespace {

class DisplayInfoTest : public ::testing::Test {
 private:
  fdf_testing::ScopedGlobalLogger logger_;
};

TEST_F(DisplayInfoTest, InitializeWithEdidValueSingleBlock) {
  const std::vector<fuchsia_images2_pixel_format_enum_value_t> pixel_formats = {
      fuchsia_images2_pixel_format_enum_value_t{fuchsia_images2::PixelFormat::kR8G8B8A8},
  };
  const raw_display_info_t raw_display_info = {
      .display_id = 1,
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = edid::kHpZr30wEdid.data(),
      .edid_bytes_count = edid::kHpZr30wEdid.size(),
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = pixel_formats.data(),
      .pixel_formats_count = pixel_formats.size(),
  };

  zx::result<fbl::RefPtr<DisplayInfo>> display_info_result = DisplayInfo::Create(raw_display_info);
  ASSERT_TRUE(display_info_result.is_ok())
      << "Failed to create DisplayInfo: "
      << zx_status_get_string(display_info_result.error_value());

  fbl::RefPtr<DisplayInfo> display_info = std::move(display_info_result).value();
  ASSERT_TRUE(display_info->edid.has_value());

  const edid::Edid& edid_base = display_info->edid->base;
  EXPECT_EQ(edid_base.edid_length(), edid::kHpZr30wEdid.size());
  EXPECT_EQ(edid_base.GetManufacturerName(), std::string("HEWLETT PACKARD"));
  EXPECT_EQ(edid_base.product_code(), 10348u);
  EXPECT_EQ(edid_base.GetDisplayProductSerialNumber(), std::string("CN413010YH"));
}

TEST_F(DisplayInfoTest, InitializeWithEdidValueMultipleBlocks) {
  const std::vector<fuchsia_images2_pixel_format_enum_value_t> pixel_formats = {
      fuchsia_images2_pixel_format_enum_value_t{fuchsia_images2::PixelFormat::kR8G8B8A8},
  };
  const raw_display_info_t raw_display_info = {
      .display_id = 1,
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = edid::kSamsungCrg9Edid.data(),
      .edid_bytes_count = edid::kSamsungCrg9Edid.size(),
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = pixel_formats.data(),
      .pixel_formats_count = pixel_formats.size(),
  };

  zx::result<fbl::RefPtr<DisplayInfo>> display_info_result = DisplayInfo::Create(raw_display_info);
  ASSERT_TRUE(display_info_result.is_ok())
      << "Failed to create DisplayInfo: "
      << zx_status_get_string(display_info_result.error_value());

  fbl::RefPtr<DisplayInfo> display_info = std::move(display_info_result).value();
  ASSERT_TRUE(display_info->edid.has_value());

  const edid::Edid& edid_base = display_info->edid->base;
  EXPECT_EQ(edid_base.edid_length(), edid::kSamsungCrg9Edid.size());
  EXPECT_EQ(edid_base.GetManufacturerName(), std::string("SAMSUNG ELECTRIC COMPANY"));
  EXPECT_EQ(edid_base.product_code(), 28754u);
  EXPECT_EQ(edid_base.GetDisplayProductSerialNumber(), std::string("H4ZR701271"));
}

TEST_F(DisplayInfoTest, InitializeWithEdidValueOfInvalidLength) {
  const std::vector<fuchsia_images2_pixel_format_enum_value_t> pixel_formats = {
      fuchsia_images2_pixel_format_enum_value_t{fuchsia_images2::PixelFormat::kR8G8B8A8},
  };

  const size_t kInvalidEdidSizeBytes = 173;
  ASSERT_LT(kInvalidEdidSizeBytes, edid::kSamsungCrg9Edid.size());

  const raw_display_info_t raw_display_info = {
      .display_id = 1,
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = edid::kSamsungCrg9Edid.data(),
      .edid_bytes_count = kInvalidEdidSizeBytes,
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = pixel_formats.data(),
      .pixel_formats_count = pixel_formats.size(),
  };

  zx::result<fbl::RefPtr<DisplayInfo>> display_info_result = DisplayInfo::Create(raw_display_info);
  ASSERT_FALSE(display_info_result.is_ok());
  EXPECT_STATUS(display_info_result.error_value(), ZX_ERR_INTERNAL);
}

TEST_F(DisplayInfoTest, InitializeWithEdidValueIncomplete) {
  const std::vector<fuchsia_images2_pixel_format_enum_value_t> pixel_formats = {
      fuchsia_images2_pixel_format_enum_value_t{fuchsia_images2::PixelFormat::kR8G8B8A8},
  };

  const size_t kIncompleteEdidSizeBytes = 128;
  ASSERT_LT(kIncompleteEdidSizeBytes, edid::kSamsungCrg9Edid.size());

  const raw_display_info_t raw_display_info = {
      .display_id = 1,
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = edid::kSamsungCrg9Edid.data(),
      .edid_bytes_count = kIncompleteEdidSizeBytes,
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = pixel_formats.data(),
      .pixel_formats_count = pixel_formats.size(),
  };

  zx::result<fbl::RefPtr<DisplayInfo>> display_info_result = DisplayInfo::Create(raw_display_info);
  ASSERT_FALSE(display_info_result.is_ok());
  EXPECT_STATUS(display_info_result.error_value(), ZX_ERR_INTERNAL);
}

TEST_F(DisplayInfoTest, InitializeWithEdidValueNonDigitalDisplay) {
  // A synthetic EDID of an analog display device.
  const std::vector<uint8_t> kEdidAnalogDisplay = {
      0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x22, 0xf0, 0x6c, 0x28, 0x01, 0x01, 0x01,
      0x01, 0x1e, 0x15, 0x01, 0x04, 0x35, 0x40, 0x28, 0x78, 0xe2, 0x8d, 0x85, 0xad, 0x4f, 0x35,
      0xb1, 0x25, 0x0e, 0x50, 0x54, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
      0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0xe2, 0x68, 0x00, 0xa0, 0xa0, 0x40,
      0x2e, 0x60, 0x30, 0x20, 0x36, 0x00, 0x81, 0x90, 0x21, 0x00, 0x00, 0x1a, 0xbc, 0x1b, 0x00,
      0xa0, 0x50, 0x20, 0x17, 0x30, 0x30, 0x20, 0x36, 0x00, 0x81, 0x90, 0x21, 0x00, 0x00, 0x1a,
      0x00, 0x00, 0x00, 0xfc, 0x00, 0x48, 0x50, 0x20, 0x5a, 0x52, 0x33, 0x30, 0x77, 0x0a, 0x20,
      0x20, 0x20, 0x20, 0x00, 0x00, 0x00, 0xff, 0x00, 0x43, 0x4e, 0x34, 0x31, 0x33, 0x30, 0x31,
      0x30, 0x59, 0x48, 0x0a, 0x20, 0x20, 0x00, 0x40};

  const std::vector<fuchsia_images2_pixel_format_enum_value_t> pixel_formats = {
      fuchsia_images2_pixel_format_enum_value_t{fuchsia_images2::PixelFormat::kR8G8B8A8},
  };

  const raw_display_info_t raw_display_info = {
      .display_id = 1,
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = kEdidAnalogDisplay.data(),
      .edid_bytes_count = kEdidAnalogDisplay.size(),
      .pixel_formats_list = pixel_formats.data(),
      .pixel_formats_count = pixel_formats.size(),
  };

  zx::result<fbl::RefPtr<DisplayInfo>> display_info_result = DisplayInfo::Create(raw_display_info);
  ASSERT_FALSE(display_info_result.is_ok());
  EXPECT_STATUS(display_info_result.error_value(), ZX_ERR_INTERNAL);
}

}  // namespace

}  // namespace display_coordinator
