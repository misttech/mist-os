// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/clock.h"

#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <lib/device-protocol/display-panel.h>

#include <gtest/gtest.h>

#include "src/graphics/display/drivers/amlogic-display/dsi.h"
#include "src/graphics/display/drivers/amlogic-display/panel-config.h"
#include "src/lib/testing/predicates/status.h"

namespace amlogic_display {

namespace {

const std::vector<display_setting_t> kDisplaySettingsForTesting = [] {
  static constexpr uint32_t kPanelIdsToTest[] = {
      PANEL_TV070WSM_FT, PANEL_P070ACB_FT,  PANEL_P101DEZ_FT,
      PANEL_TV101WXM_FT, PANEL_KD070D82_FT, PANEL_TV070WSM_ST7703I,
  };

  std::vector<display_setting_t> display_settings = {};
  for (const uint32_t panel_id : kPanelIdsToTest) {
    const display_setting_t* display_setting = GetPanelDisplaySetting(panel_id);
    ZX_ASSERT(display_setting != nullptr);
    display_settings.push_back(*display_setting);
  }
  return display_settings;
}();

// For now, simply test that timing calculations don't segfault.
TEST(AmlogicDisplayClock, PanelTiming) {
  for (const display_setting_t& t : kDisplaySettingsForTesting) {
    Clock::CalculateLcdTiming(t);
  }
}

TEST(AmlogicDisplayClock, PllTiming_ValidMode) {
  for (const display_setting_t& t : kDisplaySettingsForTesting) {
    zx::result<PllConfig> pll_r = Clock::GenerateHPLL(t);
    EXPECT_OK(pll_r.status_value());
  }
}

TEST(AmlogicDisplayClock, PllTimingHdmiPllClockRatioCalculatedCorrectly) {
  // The LCD vendor-provided display settings hardcode the HDMI PLL / DSI
  // clock ratio while the settings below requires the clock ratios to be
  // calculated automatically.
  //
  // This test ensures that the calculated clock ratios match the hardcoded
  // values removed in Ie2c4721b14a92977ef31dd2951dc4cac207cb60e.

  const display_setting_t* display_setting_tv070wsm_ft = GetPanelDisplaySetting(PANEL_TV070WSM_FT);
  ASSERT_NE(display_setting_tv070wsm_ft, nullptr);
  zx::result<PllConfig> pll_tv070wsm_ft = Clock::GenerateHPLL(*display_setting_tv070wsm_ft);
  static constexpr int kExpectedHdmiPllClockRatioTv070wsmFt = 8;
  EXPECT_OK(pll_tv070wsm_ft.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioTv070wsmFt, static_cast<int>(pll_tv070wsm_ft->clock_factor));

  const display_setting_t* display_setting_p070acb_ft = GetPanelDisplaySetting(PANEL_P070ACB_FT);
  ASSERT_NE(display_setting_p070acb_ft, nullptr);
  zx::result<PllConfig> pll_p070acb_ft = Clock::GenerateHPLL(*display_setting_p070acb_ft);
  static constexpr int kExpectedHdmiPllClockRatioP070acbFt = 8;
  EXPECT_OK(pll_p070acb_ft.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioP070acbFt, static_cast<int>(pll_p070acb_ft->clock_factor));

  const display_setting_t* display_setting_p101dez_ft = GetPanelDisplaySetting(PANEL_P101DEZ_FT);
  ASSERT_NE(display_setting_p101dez_ft, nullptr);
  zx::result<PllConfig> pll_p101dez_ft = Clock::GenerateHPLL(*display_setting_p101dez_ft);
  static constexpr int kExpectedHdmiPllClockRatioP101dezFt = 8;
  EXPECT_OK(pll_p101dez_ft.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioP101dezFt, static_cast<int>(pll_p101dez_ft->clock_factor));

  const display_setting_t* display_setting_tv101wxm_ft = GetPanelDisplaySetting(PANEL_TV101WXM_FT);
  ASSERT_NE(display_setting_tv101wxm_ft, nullptr);
  zx::result<PllConfig> pll_tv101wxm_ft = Clock::GenerateHPLL(*display_setting_tv101wxm_ft);
  static constexpr int kExpectedHdmiPllClockRatioTv101wxmFt = 8;
  EXPECT_OK(pll_tv101wxm_ft.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioTv101wxmFt, static_cast<int>(pll_tv101wxm_ft->clock_factor));
}

}  // namespace

}  // namespace amlogic_display
