// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-banjo.h"

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-protocols/cpp/mock-banjo-display-engine-listener.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
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

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayAddedWithPreferredModes) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::ModeAndId kPreferredModes[] = {
      display::ModeAndId({
          .id = display::ModeId(1),
          .mode = display::Mode({
              .active_width = 640,
              .active_height = 480,
              .refresh_rate_millihertz = 60'000,
          }),
      }),
      display::ModeAndId({
          .id = display::ModeId(2),
          .mode = display::Mode({
              .active_width = 640,
              .active_height = 480,
              .refresh_rate_millihertz = 30'000,
          }),
      }),
      display::ModeAndId({
          .id = display::ModeId(3),
          .mode = display::Mode({
              .active_width = 800,
              .active_height = 600,
              .refresh_rate_millihertz = 30'000,
          }),
      }),
  };
  static constexpr cpp20::span<const display::PixelFormat> kEmptyPixelFormats;

  static constexpr uint64_t kBanjoDisplayId = display::ToBanjoDisplayId(kDisplayId);

  mock_.ExpectOnDisplayAdded([&](const raw_display_info_t* info) {
    EXPECT_EQ(kBanjoDisplayId, info->display_id);

    ASSERT_EQ(3u, info->preferred_modes_count);

    EXPECT_EQ(640u, info->preferred_modes_list[0].h_addressable);
    EXPECT_EQ(480u, info->preferred_modes_list[0].v_addressable);
    EXPECT_EQ(640 * 480 * 60u, info->preferred_modes_list[0].pixel_clock_hz);

    EXPECT_EQ(640u, info->preferred_modes_list[1].h_addressable);
    EXPECT_EQ(480u, info->preferred_modes_list[1].v_addressable);
    EXPECT_EQ(640 * 480 * 30u, info->preferred_modes_list[1].pixel_clock_hz);

    EXPECT_EQ(800u, info->preferred_modes_list[2].h_addressable);
    EXPECT_EQ(600u, info->preferred_modes_list[2].v_addressable);
    EXPECT_EQ(800 * 600 * 30u, info->preferred_modes_list[2].pixel_clock_hz);

    cpp20::span<const uint8_t> banjo_edid_bytes(info->edid_bytes_list, info->edid_bytes_count);
    EXPECT_THAT(banjo_edid_bytes, ::testing::IsEmpty());

    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats(
        info->pixel_formats_list, info->pixel_formats_count);
    EXPECT_THAT(banjo_pixel_formats, ::testing::IsEmpty());
  });
  interface_.OnDisplayAdded(kDisplayId, kPreferredModes, kEmptyPixelFormats);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayAddedWithEdidBytes) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr cpp20::span<const display::ModeAndId> kEmptyPreferredModes;
  static constexpr cpp20::span<const display::PixelFormat> kEmptyPixelFormats;

  std::array<uint8_t, 256> edid_bytes;
  for (size_t i = 0; i < edid_bytes.size(); ++i) {
    edid_bytes[i] = static_cast<uint8_t>(i);
  }

  static constexpr uint64_t kBanjoDisplayId = display::ToBanjoDisplayId(kDisplayId);

  mock_.ExpectOnDisplayAdded([&](const raw_display_info_t* info) {
    EXPECT_EQ(kBanjoDisplayId, info->display_id);

    cpp20::span<const display_mode_t> banjo_preferred_modes(info->preferred_modes_list,
                                                            info->preferred_modes_count);
    EXPECT_THAT(banjo_preferred_modes, ::testing::IsEmpty());

    cpp20::span<const uint8_t> banjo_edid_bytes(info->edid_bytes_list, info->edid_bytes_count);
    EXPECT_THAT(banjo_edid_bytes, ::testing::ElementsAreArray(edid_bytes));

    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats(
        info->pixel_formats_list, info->pixel_formats_count);
    EXPECT_THAT(banjo_pixel_formats, ::testing::IsEmpty());
  });
  interface_.OnDisplayAdded(kDisplayId, kEmptyPreferredModes, edid_bytes, kEmptyPixelFormats);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayAddedWithPixelFormats) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr cpp20::span<const display::ModeAndId> kEmptyPreferredModes;
  static constexpr display::PixelFormat kPixelFormats[] = {
      display::PixelFormat::kB8G8R8A8, display::PixelFormat::kR8G8B8A8,
      display::PixelFormat::kR8G8B8, display::PixelFormat::kB8G8R8};

  static constexpr uint64_t kBanjoDisplayId = display::ToBanjoDisplayId(kDisplayId);
  static constexpr fuchsia_images2_pixel_format_enum_value_t kExpectedBanjoPixelFormats[] = {
      kPixelFormats[0].ToBanjo(), kPixelFormats[1].ToBanjo(), kPixelFormats[2].ToBanjo(),
      kPixelFormats[3].ToBanjo()};

  mock_.ExpectOnDisplayAdded([&](const raw_display_info_t* info) {
    EXPECT_EQ(kBanjoDisplayId, info->display_id);

    cpp20::span<const display_mode_t> banjo_preferred_modes(info->preferred_modes_list,
                                                            info->preferred_modes_count);
    EXPECT_THAT(banjo_preferred_modes, ::testing::IsEmpty());

    cpp20::span<const uint8_t> banjo_edid_bytes(info->edid_bytes_list, info->edid_bytes_count);
    EXPECT_THAT(banjo_edid_bytes, ::testing::IsEmpty());

    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats(
        info->pixel_formats_list, info->pixel_formats_count);
    EXPECT_THAT(banjo_pixel_formats, ::testing::ElementsAreArray(kExpectedBanjoPixelFormats));
  });
  interface_.OnDisplayAdded(kDisplayId, kEmptyPreferredModes, kPixelFormats);
}

TEST_F(DisplayEngineEventsBanjoTest, OnDisplayAddedWithPreferredModeAndEdidBytesAndPixelFormats) {
  std::array<uint8_t, 256> edid_bytes;
  for (size_t i = 0; i < edid_bytes.size(); ++i) {
    edid_bytes[i] = static_cast<uint8_t>(i);
  }

  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::ModeAndId kPreferredModes[] = {
      display::ModeAndId({
          .id = display::ModeId(1),
          .mode = display::Mode({
              .active_width = 640,
              .active_height = 480,
              .refresh_rate_millihertz = 60'000,
          }),
      }),
      display::ModeAndId({
          .id = display::ModeId(2),
          .mode = display::Mode({
              .active_width = 640,
              .active_height = 480,
              .refresh_rate_millihertz = 30'000,
          }),
      }),
      display::ModeAndId({
          .id = display::ModeId(3),
          .mode = display::Mode({
              .active_width = 800,
              .active_height = 600,
              .refresh_rate_millihertz = 30'000,
          }),
      }),
  };
  static constexpr display::PixelFormat kPixelFormats[] = {
      display::PixelFormat::kB8G8R8A8, display::PixelFormat::kR8G8B8A8,
      display::PixelFormat::kR8G8B8, display::PixelFormat::kB8G8R8};

  static constexpr uint64_t kBanjoDisplayId = display::ToBanjoDisplayId(kDisplayId);
  static constexpr fuchsia_images2_pixel_format_enum_value_t kExpectedBanjoPixelFormats[] = {
      kPixelFormats[0].ToBanjo(), kPixelFormats[1].ToBanjo(), kPixelFormats[2].ToBanjo(),
      kPixelFormats[3].ToBanjo()};

  mock_.ExpectOnDisplayAdded([&](const raw_display_info_t* info) {
    EXPECT_EQ(kBanjoDisplayId, info->display_id);

    ASSERT_EQ(3u, info->preferred_modes_count);

    EXPECT_EQ(640u, info->preferred_modes_list[0].h_addressable);
    EXPECT_EQ(480u, info->preferred_modes_list[0].v_addressable);
    EXPECT_EQ(640 * 480 * 60u, info->preferred_modes_list[0].pixel_clock_hz);

    EXPECT_EQ(640u, info->preferred_modes_list[1].h_addressable);
    EXPECT_EQ(480u, info->preferred_modes_list[1].v_addressable);
    EXPECT_EQ(640 * 480 * 30u, info->preferred_modes_list[1].pixel_clock_hz);

    EXPECT_EQ(800u, info->preferred_modes_list[2].h_addressable);
    EXPECT_EQ(600u, info->preferred_modes_list[2].v_addressable);
    EXPECT_EQ(800 * 600 * 30u, info->preferred_modes_list[2].pixel_clock_hz);

    cpp20::span<const uint8_t> banjo_edid_bytes(info->edid_bytes_list, info->edid_bytes_count);
    EXPECT_THAT(banjo_edid_bytes, ::testing::ElementsAreArray(edid_bytes));

    cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats(
        info->pixel_formats_list, info->pixel_formats_count);
    EXPECT_THAT(banjo_pixel_formats, ::testing::ElementsAreArray(kExpectedBanjoPixelFormats));
  });
  interface_.OnDisplayAdded(kDisplayId, kPreferredModes, edid_bytes, kPixelFormats);
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
  static constexpr display::DriverConfigStamp kConfigStamp(4242'4242);

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
  static constexpr display::DriverConfigStamp kConfigStamp(4242'4242);

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
