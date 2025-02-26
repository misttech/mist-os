// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/added-display-info.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <array>
#include <memory>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"
#include "src/graphics/display/lib/edid-values/edid-values.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

namespace {

class AddedDisplayInfoTest : public ::testing::Test {
 private:
  fdf_testing::ScopedGlobalLogger logger_;
};

TEST_F(AddedDisplayInfoTest, CreateTranscribesDisplayId) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::PixelFormat, 1> kPixelFormats = {
      display::PixelFormat::kR8G8B8A8};

  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kBanjoPixelFormats = {
      kPixelFormats[0].ToBanjo()};
  static constexpr raw_display_info_t kBanjoDisplayInfo = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .pixel_formats_list = kBanjoPixelFormats.data(),
      .pixel_formats_count = kBanjoPixelFormats.size(),
  };

  zx::result<std::unique_ptr<AddedDisplayInfo>> added_display_info_result =
      AddedDisplayInfo::Create(kBanjoDisplayInfo);
  ASSERT_OK(added_display_info_result);
  std::unique_ptr<AddedDisplayInfo> added_display_info =
      std::move(added_display_info_result).value();

  EXPECT_EQ(kDisplayId, added_display_info->display_id);
  EXPECT_THAT(added_display_info->banjo_preferred_modes, ::testing::SizeIs(0));
  EXPECT_THAT(added_display_info->edid_bytes, ::testing::SizeIs(0));
  EXPECT_THAT(added_display_info->pixel_formats, ::testing::SizeIs(1));
}

TEST_F(AddedDisplayInfoTest, CreateTranscribesPixelFormats) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::PixelFormat, 2> kPixelFormats = {
      display::PixelFormat::kR8G8B8A8, display::PixelFormat::kB8G8R8A8};

  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 2> kBanjoPixelFormats = {
      kPixelFormats[0].ToBanjo(), kPixelFormats[1].ToBanjo()};
  static constexpr raw_display_info_t kBanjoDisplayInfo = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .pixel_formats_list = kBanjoPixelFormats.data(),
      .pixel_formats_count = kBanjoPixelFormats.size(),
  };

  zx::result<std::unique_ptr<AddedDisplayInfo>> added_display_info_result =
      AddedDisplayInfo::Create(kBanjoDisplayInfo);
  ASSERT_OK(added_display_info_result);
  std::unique_ptr<AddedDisplayInfo> added_display_info =
      std::move(added_display_info_result).value();

  EXPECT_THAT(added_display_info->pixel_formats, ::testing::ElementsAreArray(kPixelFormats));

  EXPECT_EQ(kDisplayId, added_display_info->display_id);
  EXPECT_THAT(added_display_info->banjo_preferred_modes, ::testing::SizeIs(0));
  EXPECT_THAT(added_display_info->edid_bytes, ::testing::SizeIs(0));
}

TEST_F(AddedDisplayInfoTest, CreateCopiesEdidBytes) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::PixelFormat, 1> kPixelFormats = {
      display::PixelFormat::kR8G8B8A8};

  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kBanjoPixelFormats = {
      kPixelFormats[0].ToBanjo()};
  static constexpr raw_display_info_t kBanjoDisplayInfo = {
      .display_id = 42,
      .preferred_modes_list = nullptr,
      .preferred_modes_count = 0,
      .edid_bytes_list = edid::kHpZr30wEdid.data(),
      .edid_bytes_count = edid::kHpZr30wEdid.size(),
      .pixel_formats_list = kBanjoPixelFormats.data(),
      .pixel_formats_count = kBanjoPixelFormats.size(),
  };

  zx::result<std::unique_ptr<AddedDisplayInfo>> added_display_info_result =
      AddedDisplayInfo::Create(kBanjoDisplayInfo);
  ASSERT_OK(added_display_info_result);
  std::unique_ptr<AddedDisplayInfo> added_display_info =
      std::move(added_display_info_result).value();

  EXPECT_THAT(added_display_info->edid_bytes, ::testing::ElementsAreArray(edid::kHpZr30wEdid));

  EXPECT_EQ(kDisplayId, added_display_info->display_id);
  EXPECT_THAT(added_display_info->banjo_preferred_modes, ::testing::SizeIs(0));
  EXPECT_THAT(added_display_info->pixel_formats, ::testing::SizeIs(1));
}

TEST_F(AddedDisplayInfoTest, CreateCopiesDisplayModes) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::PixelFormat, 1> kPixelFormats = {
      display::PixelFormat::kR8G8B8A8};

  static constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kBanjoPixelFormats = {
      kPixelFormats[0].ToBanjo()};
  static constexpr std::array<display_mode_t, 2> kBanjoDisplayModes = {
      display_mode_t{
          .pixel_clock_hz = int64_t{60} * (640 + 10 + 20 + 30) * (480 + 40 + 50 + 60),
          .h_addressable = 640,
          .h_front_porch = 10,
          .h_sync_pulse = 20,
          .h_blanking = 10 + 20 + 30,  // HBP is 30.
          .v_addressable = 480,
          .v_front_porch = 40,
          .v_sync_pulse = 50,
          .v_blanking = 40 + 50 + 60,  // VBP is 60.
          .flags = 0,
      },
      display_mode_t{
          .pixel_clock_hz = int64_t{60} * (1024 + 10 + 20 + 30) * (768 + 40 + 50 + 60),
          .h_addressable = 1024,
          .h_front_porch = 10,
          .h_sync_pulse = 20,
          .h_blanking = 10 + 20 + 30,  // HBP is 30.
          .v_addressable = 768,
          .v_front_porch = 40,
          .v_sync_pulse = 50,
          .v_blanking = 40 + 50 + 60,  // VBP is 60.
          .flags = 0,
      },
  };
  static constexpr raw_display_info_t kBanjoDisplayInfo = {
      .display_id = 42,
      .preferred_modes_list = kBanjoDisplayModes.data(),
      .preferred_modes_count = kBanjoDisplayModes.size(),
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .pixel_formats_list = kBanjoPixelFormats.data(),
      .pixel_formats_count = kBanjoPixelFormats.size(),
  };

  zx::result<std::unique_ptr<AddedDisplayInfo>> added_display_info_result =
      AddedDisplayInfo::Create(kBanjoDisplayInfo);
  ASSERT_OK(added_display_info_result);
  std::unique_ptr<AddedDisplayInfo> added_display_info =
      std::move(added_display_info_result).value();

  // Banjo-generated structs do not have equality comparison operators, so we
  // need to check each member individually.
  ASSERT_THAT(added_display_info->banjo_preferred_modes, ::testing::SizeIs(2));

  EXPECT_EQ(int64_t{60} * (640 + 10 + 20 + 30) * (480 + 40 + 50 + 60),
            added_display_info->banjo_preferred_modes[0].pixel_clock_hz);
  EXPECT_EQ(640u, added_display_info->banjo_preferred_modes[0].h_addressable);
  EXPECT_EQ(10u, added_display_info->banjo_preferred_modes[0].h_front_porch);
  EXPECT_EQ(20u, added_display_info->banjo_preferred_modes[0].h_sync_pulse);
  EXPECT_EQ(10u + 20u + 30u, added_display_info->banjo_preferred_modes[0].h_blanking);
  EXPECT_EQ(480u, added_display_info->banjo_preferred_modes[0].v_addressable);
  EXPECT_EQ(40u, added_display_info->banjo_preferred_modes[0].v_front_porch);
  EXPECT_EQ(50u, added_display_info->banjo_preferred_modes[0].v_sync_pulse);
  EXPECT_EQ(40u + 50u + 60u, added_display_info->banjo_preferred_modes[0].v_blanking);
  EXPECT_EQ(0u, added_display_info->banjo_preferred_modes[0].flags);

  EXPECT_EQ(kDisplayId, added_display_info->display_id);
  EXPECT_THAT(added_display_info->edid_bytes, ::testing::SizeIs(0));
  EXPECT_THAT(added_display_info->pixel_formats, ::testing::SizeIs(1));
}

}  // namespace

}  // namespace display_coordinator
