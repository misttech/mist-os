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
      PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO, PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364,
      PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364,    PANEL_BOE_TV101WXM_FITIPOWER_JD9364,
      PANEL_KD_KD070D82_FITIPOWER_JD9364,        PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON,
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

  const display_setting_t* display_setting_boe_tv070wsm_fitipower_jd9364_astro =
      GetPanelDisplaySetting(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO);
  ASSERT_NE(display_setting_boe_tv070wsm_fitipower_jd9364_astro, nullptr);
  zx::result<PllConfig> pll_boe_tv070wsm_fitipower_jd9364_astro =
      Clock::GenerateHPLL(*display_setting_boe_tv070wsm_fitipower_jd9364_astro);
  static constexpr int kExpectedHdmiPllClockRatioBoeTv070wsmFitipowerJd9364Astro = 8;
  EXPECT_OK(pll_boe_tv070wsm_fitipower_jd9364_astro.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioBoeTv070wsmFitipowerJd9364Astro,
            static_cast<int>(pll_boe_tv070wsm_fitipower_jd9364_astro->clock_factor));

  const display_setting_t* display_setting_innolux_p070acb_fitipower_jd9364 =
      GetPanelDisplaySetting(PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364);
  ASSERT_NE(display_setting_innolux_p070acb_fitipower_jd9364, nullptr);
  zx::result<PllConfig> pll_innolux_p070acb_fitipower_jd9364 =
      Clock::GenerateHPLL(*display_setting_innolux_p070acb_fitipower_jd9364);
  static constexpr int kExpectedHdmiPllClockRatioInnoluxP070acbFitipowerJd9364 = 8;
  EXPECT_OK(pll_innolux_p070acb_fitipower_jd9364.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioInnoluxP070acbFitipowerJd9364,
            static_cast<int>(pll_innolux_p070acb_fitipower_jd9364->clock_factor));

  const display_setting_t* display_setting_innolux_p101dez_fitipower_jd9364 =
      GetPanelDisplaySetting(PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364);
  ASSERT_NE(display_setting_innolux_p101dez_fitipower_jd9364, nullptr);
  zx::result<PllConfig> pll_innolux_p101dez_fitipower_jd9364 =
      Clock::GenerateHPLL(*display_setting_innolux_p101dez_fitipower_jd9364);
  static constexpr int kExpectedHdmiPllClockRatioInnoluxP101dezFitipowerJd9364 = 8;
  EXPECT_OK(pll_innolux_p101dez_fitipower_jd9364.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioInnoluxP101dezFitipowerJd9364,
            static_cast<int>(pll_innolux_p101dez_fitipower_jd9364->clock_factor));

  const display_setting_t* display_setting_boe_tv101wxm_fitipower_jd9364 =
      GetPanelDisplaySetting(PANEL_BOE_TV101WXM_FITIPOWER_JD9364);
  ASSERT_NE(display_setting_boe_tv101wxm_fitipower_jd9364, nullptr);
  zx::result<PllConfig> pll_boe_tv101wxm_fitipower_jd9364 =
      Clock::GenerateHPLL(*display_setting_boe_tv101wxm_fitipower_jd9364);
  static constexpr int kExpectedHdmiPllClockRatioBoeTv101wxmFitipowerJd9364 = 8;
  EXPECT_OK(pll_boe_tv101wxm_fitipower_jd9364.status_value());
  EXPECT_EQ(kExpectedHdmiPllClockRatioBoeTv101wxmFitipowerJd9364,
            static_cast<int>(pll_boe_tv101wxm_fitipower_jd9364->clock_factor));
}

}  // namespace

}  // namespace amlogic_display
