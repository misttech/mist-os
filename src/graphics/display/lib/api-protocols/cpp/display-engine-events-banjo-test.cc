// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-banjo.h"

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-protocols/cpp/mock-banjo-display-engine-listener.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display {

namespace {

class DisplayEngineEventsBanjoTest : public ::testing::Test {
 public:
  void SetUp() override { banjo_adapter_.SetListener(&display_engine_listener_protocol); }

  void TearDown() override {
    banjo_adapter_.SetListener(nullptr);
    mock_.CheckAllCallsReplayed();
  }

 protected:
  fdf_testing::ScopedGlobalLogger logger_;

  display::testing::MockBanjoDisplayEngineListener mock_;
  const display_engine_listener_protocol_t display_engine_listener_protocol = mock_.GetProtocol();
  DisplayEngineEventsBanjo banjo_adapter_;
  DisplayEngineEventsInterface& interface_{banjo_adapter_};
};

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayAddedWithPreferredMode) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::ModeAndId kDisplayModeAndId({
      .id = display::ModeId(1),
      .mode = display::Mode({
          .active_width = 640,
          .active_height = 480,
          .refresh_rate_millihertz = 60'000,
      }),
  });
  static constexpr cpp20::span<const display::ModeAndId> kPreferredModes(&kDisplayModeAndId, 1);
  static constexpr display::PixelFormat kPixelFormat = display::PixelFormat::kB8G8R8A8;
  static constexpr cpp20::span<const display::PixelFormat> kPixelFormats(&kPixelFormat, 1);

  static constexpr uint64_t banjo_display_id = display::ToBanjoDisplayId(kDisplayId);
  static constexpr fuchsia_images2_pixel_format_enum_value_t expected_banjo_pixel_format =
      kPixelFormat.ToBanjo();
  static constexpr cpp20::span<const fuchsia_images2_pixel_format_enum_value_t>
      expected_banjo_pixel_formats(&expected_banjo_pixel_format, 1);

  mock_.ExpectOnDisplayAdded([&](const raw_display_info_t* info) {
    EXPECT_EQ(banjo_display_id, info->display_id);

    ASSERT_EQ(1u, info->preferred_modes_count);
    EXPECT_EQ(640u, info->preferred_modes_list[0].h_addressable);
    EXPECT_EQ(480u, info->preferred_modes_list[0].v_addressable);
    EXPECT_EQ(640 * 480 * 60u, info->preferred_modes_list[0].pixel_clock_hz);

    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats(
        info->pixel_formats_list, info->pixel_formats_count);
    EXPECT_THAT(banjo_pixel_formats, ::testing::ElementsAreArray(expected_banjo_pixel_formats));
  });
  interface_.OnDisplayAdded(kDisplayId, kPreferredModes, kPixelFormats);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayAddedWithNoListener) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::ModeAndId kDisplayModeAndId({
      .id = display::ModeId(1),
      .mode = display::Mode({
          .active_width = 640,
          .active_height = 480,
          .refresh_rate_millihertz = 60'000,
      }),
  });
  static constexpr cpp20::span<const display::ModeAndId> kPreferredModes(&kDisplayModeAndId, 1);
  static constexpr display::PixelFormat kPixelFormat = display::PixelFormat::kB8G8R8A8;
  static constexpr cpp20::span<const display::PixelFormat> kPixelFormats(&kPixelFormat, 1);

  banjo_adapter_.SetListener(nullptr);
  interface_.OnDisplayAdded(kDisplayId, kPreferredModes, kPixelFormats);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayRemoved) {
  static constexpr display::DisplayId kDisplayId(42);

  mock_.ExpectOnDisplayRemoved(
      [&](uint64_t banjo_display_id) { EXPECT_EQ(42u, banjo_display_id); });
  interface_.OnDisplayRemoved(kDisplayId);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayRemovedWithNoListener) {
  static constexpr display::DisplayId kDisplayId(42);

  banjo_adapter_.SetListener(nullptr);
  interface_.OnDisplayRemoved(kDisplayId);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayVsync) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr zx::time kTimestamp(zx_time_t{42'4242});
  static constexpr display::ConfigStamp kConfigStamp(4242'4242);

  mock_.ExpectOnDisplayVsync([&](uint64_t banjo_display_id, zx_time_t banjo_timestamp,
                                 const config_stamp_t* banjo_config_stamp) {
    EXPECT_EQ(42u, banjo_display_id);
    EXPECT_EQ(42'4242u, banjo_timestamp);
    EXPECT_EQ(4242'4242u, banjo_config_stamp->value);
  });
  interface_.OnDisplayVsync(kDisplayId, kTimestamp, kConfigStamp);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayVsyncWithNoListener) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr zx::time kTimestamp(zx_time_t{42'4242});
  static constexpr display::ConfigStamp kConfigStamp(4242'4242);

  banjo_adapter_.SetListener(nullptr);
  interface_.OnDisplayVsync(kDisplayId, kTimestamp, kConfigStamp);
}

TEST_F(DisplayEngineEventsBanjoTest, OnCaptureComplete) {
  mock_.ExpectOnCaptureComplete([]() {});
  interface_.OnCaptureComplete();
}

TEST_F(DisplayEngineEventsBanjoTest, OnCaptureCompleteWithNoListener) {
  banjo_adapter_.SetListener(nullptr);
  interface_.OnCaptureComplete();
}

}  // namespace

}  // namespace display
