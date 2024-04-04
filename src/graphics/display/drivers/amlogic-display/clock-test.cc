// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/clock.h"

#include <lib/device-protocol/display-panel.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>

#include <cstdint>
#include <vector>

#include <gtest/gtest.h>

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"
#include "src/lib/testing/predicates/status.h"

namespace amlogic_display {

namespace {

// All the PanelConfig pointers are non-null in the returned array.
const std::vector<const PanelConfig*> kPanelConfigsForTesting = [] {
  std::vector<const PanelConfig*> panel_configs;
  const uint32_t kPanelIds[] = {
      PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO, PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364,
      PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364,    PANEL_BOE_TV101WXM_FITIPOWER_JD9364,
      PANEL_KD_KD070D82_FITIPOWER_JD9364,        PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON,
  };
  for (const uint32_t panel : kPanelIds) {
    const PanelConfig* panel_config = GetPanelConfig(panel);
    ZX_ASSERT(panel_config != nullptr);
    panel_configs.push_back(panel_config);
  }
  return panel_configs;
}();

// For now, simply test that timing calculations don't segfault.
TEST(AmlogicDisplayClock, PanelTiming) {
  for (const PanelConfig* panel_config : kPanelConfigsForTesting) {
    ASSERT_NE(panel_config, nullptr);
    SCOPED_TRACE(::testing::Message() << panel_config->name);
    Clock::CalculateLcdTiming(panel_config->display_timing);
  }
}

TEST(AmlogicDisplayClock, PllTiming_ValidMode) {
  for (const PanelConfig* panel_config : kPanelConfigsForTesting) {
    ASSERT_NE(panel_config, nullptr);
    SCOPED_TRACE(::testing::Message() << panel_config->name);
    zx::result<HdmiPllConfigForMipiDsi> pll_r =
        Clock::GenerateHPLL(panel_config->display_timing.pixel_clock_frequency_hz,
                            panel_config->maximum_per_data_lane_bit_per_second());
    EXPECT_OK(pll_r.status_value());
  }
}

// The LCD vendor-provided display settings hardcode the HDMI PLL / DSI
// clock ratio while the settings below requires the clock ratios to be
// calculated automatically.
//
// The following tests ensure that the calculated clock ratios match the
// hardcoded values removed in Ie2c4721b14a92977ef31dd2951dc4cac207cb60e.

TEST(PllTimingHdmiPllClockRatioCalculatedCorrectly, BoeTv070wsmFitipowerJd9364Astro) {
  const PanelConfig* panel_config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO);
  ASSERT_NE(panel_config, nullptr);
  zx::result<HdmiPllConfigForMipiDsi> pll_config =
      Clock::GenerateHPLL(panel_config->display_timing.pixel_clock_frequency_hz,
                          panel_config->maximum_per_data_lane_bit_per_second());
  static constexpr int kExpectedHdmiPllClockRatio = 8;
  EXPECT_OK(pll_config.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatio, static_cast<int>(pll_config->clock_factor));
}

TEST(PllTimingHdmiPllClockRatioCalculatedCorrectly, InnoluxP070acbFitipowerJd9364) {
  const PanelConfig* panel_config = GetPanelConfig(PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364);
  ASSERT_NE(panel_config, nullptr);
  zx::result<HdmiPllConfigForMipiDsi> pll_config =
      Clock::GenerateHPLL(panel_config->display_timing.pixel_clock_frequency_hz,
                          panel_config->maximum_per_data_lane_bit_per_second());
  static constexpr int kExpectedHdmiPllClockRatio = 8;
  EXPECT_OK(pll_config.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatio, static_cast<int>(pll_config->clock_factor));
}

TEST(PllTimingHdmiPllClockRatioCalculatedCorrectly, InnoluxP101dezFitipowerJd9364) {
  const PanelConfig* panel_config = GetPanelConfig(PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364);
  ASSERT_NE(panel_config, nullptr);
  zx::result<HdmiPllConfigForMipiDsi> pll_config =
      Clock::GenerateHPLL(panel_config->display_timing.pixel_clock_frequency_hz,
                          panel_config->maximum_per_data_lane_bit_per_second());
  static constexpr int kExpectedHdmiPllClockRatio = 8;
  EXPECT_OK(pll_config.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatio, static_cast<int>(pll_config->clock_factor));
}

TEST(PllTimingHdmiPllClockRatioCalculatedCorrectly, BoeTv101wxmFitipowerJd9364) {
  const PanelConfig* panel_config = GetPanelConfig(PANEL_BOE_TV101WXM_FITIPOWER_JD9364);
  ASSERT_NE(panel_config, nullptr);
  zx::result<HdmiPllConfigForMipiDsi> pll_config =
      Clock::GenerateHPLL(panel_config->display_timing.pixel_clock_frequency_hz,
                          panel_config->maximum_per_data_lane_bit_per_second());
  static constexpr int kExpectedHdmiPllClockRatio = 8;
  EXPECT_OK(pll_config.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatio, static_cast<int>(pll_config->clock_factor));
}

}  // namespace

}  // namespace amlogic_display
