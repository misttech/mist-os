// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

#include <lib/device-protocol/display-panel.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace amlogic_display {

namespace {

TEST(PanelConfig, BoeTv070wsmFitipowerJd9364Astro) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "BOE_TV070WSM_FITIPOWER_JD9364_ASTRO");
}

TEST(PanelConfig, InnoluxP070acbFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "INNOLUX_P070ACB_FITIPOWER_JD9364");
}

TEST(PanelConfig, BoeTv101wxmFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV101WXM_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "BOE_TV101WXM_FITIPOWER_JD9364");
}

TEST(PanelConfig, InnoluxP101dezFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "INNOLUX_P101DEZ_FITIPOWER_JD9364");
}

TEST(PanelConfig, BoeTv101wxmFitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV101WXM_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "BOE_TV101WXM_FITIPOWER_JD9365");
}

TEST(PanelConfig, BoeTv070wsmFitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "BOE_TV070WSM_FITIPOWER_JD9365");
}

TEST(PanelConfig, KdKd070d82FitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_KD_KD070D82_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "KD_KD070D82_FITIPOWER_JD9364");
}

TEST(PanelConfig, KdKd070d82FitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_KD_KD070D82_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "KD_KD070D82_FITIPOWER_JD9365");
}

TEST(PanelConfig, MicrotechMtf050fhdi03NovatekNt35596) {
  const PanelConfig* config = GetPanelConfig(PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "MICROTECH_MTF050FHDI03_NOVATEK_NT35596");
}

TEST(PanelConfig, BoeTv070wsmFitipowerJd9364Nelson) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON);
  ASSERT_NE(config, nullptr);
  EXPECT_STREQ(config->name, "BOE_TV070WSM_FITIPOWER_JD9364_NELSON");
}

TEST(PanelConfig, InvalidPanels) {
  const PanelConfig* config_0x04 = GetPanelConfig(0x04);
  EXPECT_EQ(config_0x04, nullptr);

  const PanelConfig* config_0x05 = GetPanelConfig(0x05);
  EXPECT_EQ(config_0x05, nullptr);

  const PanelConfig* config_0x06 = GetPanelConfig(0x06);
  EXPECT_EQ(config_0x06, nullptr);

  const PanelConfig* config_overly_large = GetPanelConfig(0x0e);
  EXPECT_EQ(config_overly_large, nullptr);

  const PanelConfig* config_unknown = GetPanelConfig(PANEL_UNKNOWN);
  EXPECT_EQ(config_unknown, nullptr);
}

TEST(PanelDisplaySetting, InnoluxP070acbFitipowerJd9364) {
  const display_setting_t* timing = GetPanelDisplaySetting(PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 600u);
  EXPECT_EQ(timing->v_active, 1024u);
}

TEST(PanelDisplaySetting, BoeTv070wsmFitipowerJd9364Astro) {
  const display_setting_t* timing =
      GetPanelDisplaySetting(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 600u);
  EXPECT_EQ(timing->v_active, 1024u);
}

TEST(PanelDisplaySetting, KdKd070d82FitipowerJd9364) {
  const display_setting_t* timing = GetPanelDisplaySetting(PANEL_KD_KD070D82_FITIPOWER_JD9364);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 600u);
  EXPECT_EQ(timing->v_active, 1024u);
}

TEST(PanelDisplaySetting, KdKd070d82FitipowerJd9365) {
  const display_setting_t* timing = GetPanelDisplaySetting(PANEL_KD_KD070D82_FITIPOWER_JD9365);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 600u);
  EXPECT_EQ(timing->v_active, 1024u);
}

TEST(PanelDisplaySetting, BoeTv070wsmFitipowerJd9365) {
  const display_setting_t* timing = GetPanelDisplaySetting(PANEL_BOE_TV070WSM_FITIPOWER_JD9365);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 600u);
  EXPECT_EQ(timing->v_active, 1024u);
}

TEST(PanelDisplaySetting, InnoluxP101dezFitipowerJd9364) {
  const display_setting_t* timing = GetPanelDisplaySetting(PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 800u);
  EXPECT_EQ(timing->v_active, 1280u);
}

TEST(PanelDisplaySetting, BoeTv101wxmFitipowerJd9364) {
  const display_setting_t* timing = GetPanelDisplaySetting(PANEL_BOE_TV101WXM_FITIPOWER_JD9364);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 800u);
  EXPECT_EQ(timing->v_active, 1280u);
}

TEST(PanelDisplaySetting, BoeTv101wxmFitipowerJd9365) {
  const display_setting_t* timing = GetPanelDisplaySetting(PANEL_BOE_TV101WXM_FITIPOWER_JD9365);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 800u);
  EXPECT_EQ(timing->v_active, 1280u);
}

TEST(PanelDisplaySetting, MicrotechMtf050fhdi03NovatekNt35596) {
  const display_setting_t* timing =
      GetPanelDisplaySetting(PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 1080u);
  EXPECT_EQ(timing->v_active, 1920u);
}

TEST(PanelDisplaySetting, BoeTv070wsmFitipowerJd9364Nelson) {
  const display_setting_t* timing =
      GetPanelDisplaySetting(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON);
  ASSERT_NE(timing, nullptr);
  EXPECT_EQ(timing->h_active, 600u);
  EXPECT_EQ(timing->v_active, 1024u);
}

TEST(PanelDisplaySetting, InvalidPanels) {
  const display_setting_t* timing_0x04 = GetPanelDisplaySetting(0x04);
  EXPECT_EQ(timing_0x04, nullptr);

  const display_setting_t* timing_0x05 = GetPanelDisplaySetting(0x05);
  EXPECT_EQ(timing_0x05, nullptr);

  const display_setting_t* timing_0x06 = GetPanelDisplaySetting(0x06);
  EXPECT_EQ(timing_0x06, nullptr);

  const display_setting_t* timing_0x0b = GetPanelDisplaySetting(0x0b);
  EXPECT_EQ(timing_0x0b, nullptr);

  const display_setting_t* timing_overly_large = GetPanelDisplaySetting(0x0e);
  EXPECT_EQ(timing_overly_large, nullptr);

  const display_setting_t* timing_unknown = GetPanelDisplaySetting(PANEL_UNKNOWN);
  EXPECT_EQ(timing_unknown, nullptr);
}

TEST(RefreshRate, InnoluxP070acbFitipowerJd9364) {
  const display_setting_t* setting = GetPanelDisplaySetting(PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, BoeTv070wsmFitipowerJd9364Astro) {
  const display_setting_t* setting =
      GetPanelDisplaySetting(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, KdKd070d82FitipowerJd9364) {
  const display_setting_t* setting = GetPanelDisplaySetting(PANEL_KD_KD070D82_FITIPOWER_JD9364);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, KdKd070d82FitipowerJd9365) {
  const display_setting_t* setting = GetPanelDisplaySetting(PANEL_KD_KD070D82_FITIPOWER_JD9365);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, BoeTv070wsmFitipowerJd9365) {
  const display_setting_t* setting = GetPanelDisplaySetting(PANEL_BOE_TV070WSM_FITIPOWER_JD9365);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, InnoluxP101dezFitipowerJd9364) {
  const display_setting_t* setting = GetPanelDisplaySetting(PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, BoeTv101wxmFitipowerJd9364) {
  const display_setting_t* setting = GetPanelDisplaySetting(PANEL_BOE_TV101WXM_FITIPOWER_JD9364);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, BoeTv101wxmFitipowerJd9365) {
  const display_setting_t* setting = GetPanelDisplaySetting(PANEL_BOE_TV101WXM_FITIPOWER_JD9365);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(RefreshRate, MicrotechMtf050fhdi03NovatekNt35596) {
  const display_setting_t* setting =
      GetPanelDisplaySetting(PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 55'428);
}

TEST(RefreshRate, BoeTv070wsmFitipowerJd9364Nelson) {
  const display_setting_t* setting =
      GetPanelDisplaySetting(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON);
  ASSERT_NE(setting, nullptr);
  const display::DisplayTiming timing = display::ToDisplayTiming(*setting);
  EXPECT_EQ(timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

}  // namespace

}  // namespace amlogic_display
