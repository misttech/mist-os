// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/mode.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr Mode kVga60Fps({
    .active_width = 640,
    .active_height = 480,
    .refresh_rate_millihertz = 60'000,
});

constexpr Mode kVga60Fps2({
    .active_width = 640,
    .active_height = 480,
    .refresh_rate_millihertz = 60'000,
});

constexpr Mode kQvga30Fps({
    .active_width = 240,
    .active_height = 320,
    .refresh_rate_millihertz = 30'000,
});

TEST(ModeTest, EqualityIsReflexive) {
  EXPECT_EQ(kVga60Fps, kVga60Fps);
  EXPECT_EQ(kVga60Fps2, kVga60Fps2);
  EXPECT_EQ(kQvga30Fps, kQvga30Fps);
}

TEST(ModeTest, EqualityIsSymmetric) {
  EXPECT_EQ(kVga60Fps, kVga60Fps2);
  EXPECT_EQ(kVga60Fps2, kVga60Fps);
}

TEST(ModeTest, EqualityForDifferentWidths) {
  static constexpr Mode kSmallSquare30Fps({
      .active_width = 240,
      .active_height = 240,
      .refresh_rate_millihertz = 30'000,
  });
  EXPECT_NE(kQvga30Fps, kSmallSquare30Fps);
  EXPECT_NE(kSmallSquare30Fps, kQvga30Fps);
}

TEST(ModeTest, EqualityForDifferentHeights) {
  static constexpr Mode kLargeSquare30Fps({
      .active_width = 320,
      .active_height = 320,
      .refresh_rate_millihertz = 30'000,
  });
  EXPECT_NE(kQvga30Fps, kLargeSquare30Fps);
  EXPECT_NE(kLargeSquare30Fps, kQvga30Fps);
}

TEST(ModeTest, EqualityForDifferentRefreshRates) {
  static constexpr Mode kQvga60Fps({
      .active_width = 240,
      .active_height = 320,
      .refresh_rate_millihertz = 60'000,
  });
  EXPECT_NE(kQvga30Fps, kQvga60Fps);
  EXPECT_NE(kQvga60Fps, kQvga30Fps);
}

TEST(ModeTest, FromDesignatedInitializer) {
  static constexpr Mode mode({
      .active_width = 640,
      .active_height = 480,
      .refresh_rate_millihertz = 60'000,
  });
  EXPECT_EQ(640, mode.active_area().width());
  EXPECT_EQ(480, mode.active_area().height());
  EXPECT_EQ(Dimensions({.width = 640, .height = 480}), mode.active_area());
  EXPECT_EQ(60'000, mode.refresh_rate_millihertz());
}

TEST(ModeTest, FromFidlMode) {
  static constexpr fuchsia_hardware_display_types::wire::Mode fidl_mode = {
      .active_area = {.width = 640, .height = 480},
      .refresh_rate_millihertz = 60'000,
  };

  static constexpr Mode mode = Mode::From(fidl_mode);
  EXPECT_EQ(640, mode.active_area().width());
  EXPECT_EQ(480, mode.active_area().height());
  EXPECT_EQ(Dimensions({.width = 640, .height = 480}), mode.active_area());
  EXPECT_EQ(60'000, mode.refresh_rate_millihertz());
}

TEST(ModeTest, FromBanjoMode) {
  static constexpr display_mode_t banjo_mode = {
      .pixel_clock_hz = int64_t{60} * 640 * 480,
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  };

  static constexpr Mode mode = Mode::From(banjo_mode);
  EXPECT_EQ(640, mode.active_area().width());
  EXPECT_EQ(480, mode.active_area().height());
  EXPECT_EQ(Dimensions({.width = 640, .height = 480}), mode.active_area());
  EXPECT_EQ(60'000, mode.refresh_rate_millihertz());
}

TEST(ModeTest, ToFidlMode) {
  static constexpr Mode mode({
      .active_width = 640,
      .active_height = 480,
      .refresh_rate_millihertz = 60'000,
  });

  static constexpr fuchsia_hardware_display_types::wire::Mode fidl_mode = mode.ToFidl();
  EXPECT_EQ(640u, fidl_mode.active_area.width);
  EXPECT_EQ(480u, fidl_mode.active_area.height);
  EXPECT_EQ(60'000u, fidl_mode.refresh_rate_millihertz);
  EXPECT_EQ(fuchsia_hardware_display_types::wire::ModeFlags(), fidl_mode.flags);
}

TEST(ModeTest, ToBanjoMode) {
  static constexpr Mode mode({
      .active_width = 640,
      .active_height = 480,
      .refresh_rate_millihertz = 60'000,
  });

  static constexpr display_mode_t banjo_mode = mode.ToBanjo();
  EXPECT_EQ(60u * 640 * 480, banjo_mode.pixel_clock_hz);
  EXPECT_EQ(640u, banjo_mode.h_addressable);
  EXPECT_EQ(0u, banjo_mode.h_front_porch);
  EXPECT_EQ(0u, banjo_mode.h_sync_pulse);
  EXPECT_EQ(0u, banjo_mode.h_blanking);
  EXPECT_EQ(480u, banjo_mode.v_addressable);
  EXPECT_EQ(0u, banjo_mode.v_front_porch);
  EXPECT_EQ(0u, banjo_mode.v_sync_pulse);
  EXPECT_EQ(0u, banjo_mode.v_blanking);
  EXPECT_EQ(0u, banjo_mode.flags);
}

TEST(ModeTest, IsValidFidlVga60Fps) {
  EXPECT_TRUE(Mode::IsValid(fuchsia_hardware_display_types::wire::Mode{
      .active_area = {.width = 640, .height = 480},
      .refresh_rate_millihertz = 60'000,
      .flags = fuchsia_hardware_display_types::wire::ModeFlags(),
  }));
}

TEST(ModeTest, IsValidFidlLargeWidth) {
  EXPECT_FALSE(Mode::IsValid(fuchsia_hardware_display_types::wire::Mode{
      .active_area = {.width = 1'000'000, .height = 480},
      .refresh_rate_millihertz = 60'000,
      .flags = fuchsia_hardware_display_types::wire::ModeFlags(),
  }));
}

TEST(ModeTest, IsValidFidlLargeHeight) {
  EXPECT_FALSE(Mode::IsValid(fuchsia_hardware_display_types::wire::Mode{
      .active_area = {.width = 640, .height = 1'000'000},
      .refresh_rate_millihertz = 60'000,
      .flags = fuchsia_hardware_display_types::wire::ModeFlags(),
  }));
}

TEST(ModeTest, IsValidFidlLargeRefreshRate) {
  EXPECT_FALSE(Mode::IsValid(fuchsia_hardware_display_types::wire::Mode{
      .active_area = {.width = 640, .height = 480},
      .refresh_rate_millihertz = 10'000'000,
      .flags = fuchsia_hardware_display_types::wire::ModeFlags(),
  }));
}

TEST(ModeTest, IsValidFidlNonZeroFlags) {
  EXPECT_FALSE(Mode::IsValid(fuchsia_hardware_display_types::wire::Mode{
      .active_area = {.width = 640, .height = 480},
      .refresh_rate_millihertz = 10'000'000,
      .flags = static_cast<fuchsia_hardware_display_types::wire::ModeFlags>(1),
  }));
}

TEST(ModeTest, IsValidBanjoVga60Fps) {
  EXPECT_TRUE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * 640 * 480,
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoLargeWidth) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * 1'000'000 * 480,
      .h_addressable = 1'000'000,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoLargeHeight) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * 640 * 1'000'000,
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 1'000'000,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoLargeRefreshRate) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{10'000} * 640 * 480,
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoNonZeroHorizontalFrontPorch) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * (640 + 10) * 480,
      .h_addressable = 640,
      .h_front_porch = 10,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoNonZeroHorizontalSyncPulse) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * (640 + 10) * 480,
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 10,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoNonZeroHorizontalBackPorch) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * (640 + 10) * 480,
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 10,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoNonZeroVerticalFrontPorch) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * 640 * (480 + 10),
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 10,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoNonZeroVerticalSyncPulse) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * 640 * (480 + 10),
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 00,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 10,
      .v_blanking = 0,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoNonZeroVerticalBackPorch) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * 640 * (480 + 10),
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 10,
      .flags = 0,
  }));
}

TEST(ModeTest, IsValidBanjoNonZeroFlags) {
  EXPECT_FALSE(Mode::IsValid(display_mode_t{
      .pixel_clock_hz = int64_t{60} * 640 * 480,
      .h_addressable = 640,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 480,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 1,
  }));
}

}  // namespace

}  // namespace display
