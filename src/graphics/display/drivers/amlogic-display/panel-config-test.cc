// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

#include <lib/device-protocol/display-panel.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/display-timing.h"

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

  const PanelConfig* config_0x0b = GetPanelConfig(0x0b);
  EXPECT_EQ(config_0x0b, nullptr);

  const PanelConfig* config_overly_large = GetPanelConfig(0x0e);
  EXPECT_EQ(config_overly_large, nullptr);

  const PanelConfig* config_unknown = GetPanelConfig(PANEL_UNKNOWN);
  EXPECT_EQ(config_unknown, nullptr);
}

TEST(PanelConfigTimingIsValid, BoeTv070wsmFitipowerJd9364Astro) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, InnoluxP070acbFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, BoeTv101wxmFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV101WXM_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, InnoluxP101dezFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, BoeTv101wxmFitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV101WXM_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, BoeTv070wsmFitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, KdKd070d82FitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_KD_KD070D82_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, KdKd070d82FitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_KD_KD070D82_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, MicrotechMtf050fhdi03NovatekNt35596) {
  const PanelConfig* config = GetPanelConfig(PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigTimingIsValid, BoeTv070wsmFitipowerJd9364Nelson) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON);
  ASSERT_NE(config, nullptr);

  EXPECT_EQ(display::FieldsPerFrame::kProgressive, config->display_timing.fields_per_frame);
  EXPECT_EQ(false, config->display_timing.vblank_alternates);
  EXPECT_EQ(0, config->display_timing.pixel_repetition);
  EXPECT_TRUE(config->display_timing.IsValid());
}

TEST(PanelConfigRefreshRateMatchesSpec, BoeTv070wsmFitipowerJd9364Astro) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, InnoluxP070acbFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, BoeTv101wxmFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV101WXM_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, InnoluxP101dezFitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, BoeTv101wxmFitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV101WXM_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, BoeTv070wsmFitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, KdKd070d82FitipowerJd9364) {
  const PanelConfig* config = GetPanelConfig(PANEL_KD_KD070D82_FITIPOWER_JD9364);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, KdKd070d82FitipowerJd9365) {
  const PanelConfig* config = GetPanelConfig(PANEL_KD_KD070D82_FITIPOWER_JD9365);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfigRefreshRateMatchesSpec, BoeTv070wsmFitipowerJd9364Nelson) {
  const PanelConfig* config = GetPanelConfig(PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON);
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->display_timing.vertical_field_refresh_rate_millihertz(), 60'000);
}

TEST(PanelConfig, ToDisplaySetting) {
  const PanelConfig config = {
      .name = "Test Panel",
      .dsi_on = {},
      .dsi_off = {},
      .power_on = {},
      .power_off = {},

      .dphy_data_lane_count = 4,
      .maximum_dphy_clock_lane_frequency_hz = int64_t{0x13'13} * 1'000'000,

      .display_timing =
          {
              .horizontal_active_px = 0x0f'0f,
              .horizontal_front_porch_px = 0x0a'0a,
              .horizontal_sync_width_px = 0x01'01,
              .horizontal_back_porch_px = 0x02'02,
              .vertical_active_lines = 0x0b'0b,
              .vertical_front_porch_lines = 0x03'03,
              .vertical_sync_width_lines = 0x04'04,
              .vertical_back_porch_lines = 0x05'05,
              .pixel_clock_frequency_hz = 0x1a'1a'1a'1a,
              .fields_per_frame = display::FieldsPerFrame::kProgressive,
              .hsync_polarity = display::SyncPolarity::kPositive,
              .vsync_polarity = display::SyncPolarity::kPositive,
              .vblank_alternates = false,
              .pixel_repetition = 0,
          },
  };

  const display_setting_t display_setting = ToDisplaySetting(config);
  EXPECT_EQ(display_setting.lane_num, 4u);
  EXPECT_EQ(display_setting.bit_rate_max, 0x26'26u);
  EXPECT_EQ(display_setting.lcd_clock, 0x1a'1a'1a'1au);
  EXPECT_EQ(display_setting.h_active, 0x0f'0fu);
  EXPECT_EQ(display_setting.v_active, 0x0b'0bu);
  EXPECT_EQ(display_setting.h_period, 0x1c'1cu);
  EXPECT_EQ(display_setting.v_period, 0x17'17u);
  EXPECT_EQ(display_setting.hsync_width, 0x01'01u);
  EXPECT_EQ(display_setting.hsync_bp, 0x02'02u);
  EXPECT_EQ(display_setting.hsync_pol, 1u);
  EXPECT_EQ(display_setting.vsync_width, 0x04'04u);
  EXPECT_EQ(display_setting.vsync_bp, 0x05'05u);
  EXPECT_EQ(display_setting.vsync_pol, 1u);
}

TEST(PanelConfig, ToDisplaySettingSyncPolarity) {
  {
    const PanelConfig config = {
        .name = "Test Panel",
        .dsi_on = {},
        .dsi_off = {},
        .power_on = {},
        .power_off = {},

        .dphy_data_lane_count = 4,
        .maximum_dphy_clock_lane_frequency_hz = int64_t{0x13'13} * 1'000'000,

        .display_timing =
            {
                .horizontal_active_px = 0x0f'0f,
                .horizontal_front_porch_px = 0x0a'0a,
                .horizontal_sync_width_px = 0x01'01,
                .horizontal_back_porch_px = 0x02'02,
                .vertical_active_lines = 0x0b'0b,
                .vertical_front_porch_lines = 0x03'03,
                .vertical_sync_width_lines = 0x04'04,
                .vertical_back_porch_lines = 0x05'05,
                .pixel_clock_frequency_hz = 0x1a'1a'1a'1a,
                .fields_per_frame = display::FieldsPerFrame::kProgressive,
                .hsync_polarity = display::SyncPolarity::kPositive,
                .vsync_polarity = display::SyncPolarity::kNegative,
                .vblank_alternates = false,
                .pixel_repetition = 0,
            },
    };

    const display_setting_t display_setting = ToDisplaySetting(config);
    EXPECT_EQ(display_setting.hsync_pol, 1u);
    EXPECT_EQ(display_setting.vsync_pol, 0u);
  }

  {
    const PanelConfig config = {
        .name = "Test Panel",
        .dsi_on = {},
        .dsi_off = {},
        .power_on = {},
        .power_off = {},

        .dphy_data_lane_count = 4,
        .maximum_dphy_clock_lane_frequency_hz = int64_t{0x13'13} * 1'000'000,

        .display_timing =
            {
                .horizontal_active_px = 0x0f'0f,
                .horizontal_front_porch_px = 0x0a'0a,
                .horizontal_sync_width_px = 0x01'01,
                .horizontal_back_porch_px = 0x02'02,
                .vertical_active_lines = 0x0b'0b,
                .vertical_front_porch_lines = 0x03'03,
                .vertical_sync_width_lines = 0x04'04,
                .vertical_back_porch_lines = 0x05'05,
                .pixel_clock_frequency_hz = 0x1a'1a'1a'1a,
                .fields_per_frame = display::FieldsPerFrame::kProgressive,
                .hsync_polarity = display::SyncPolarity::kNegative,
                .vsync_polarity = display::SyncPolarity::kPositive,
                .vblank_alternates = false,
                .pixel_repetition = 0,
            },
    };

    const display_setting_t display_setting = ToDisplaySetting(config);
    EXPECT_EQ(display_setting.hsync_pol, 0u);
    EXPECT_EQ(display_setting.vsync_pol, 1u);
  }
}

}  // namespace

}  // namespace amlogic_display
