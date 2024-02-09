// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

#include <lib/device-protocol/display-panel.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include "src/graphics/display/drivers/amlogic-display/initcodes-inl.h"

namespace amlogic_display {

namespace {

constexpr PanelConfig kTv070wsmFtAstroPanelConfig = {
    .name = "TV070WSM_FT_ASTRO",
    .dsi_on = lcd_init_sequence_TV070WSM_FT_ASTRO,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kTv070wsmFitipowerDisplaySetting = {
    .lane_num = 4,
    .bit_rate_max = 360,
    .lcd_clock = 44226000,
    .h_active = 600,
    .v_active = 1024,
    .h_period = 700,
    .v_period = 1053,
    .hsync_width = 24,
    .hsync_bp = 36,
    .hsync_pol = 0,
    .vsync_width = 2,
    .vsync_bp = 8,
    .vsync_pol = 0,
};

constexpr PanelConfig kP070acbFtPanelConfig = {
    .name = "P070ACB_FT",
    .dsi_on = lcd_init_sequence_P070ACB_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kP070acbFitipowerDisplaySetting = {
    .lane_num = 4,
    .bit_rate_max = 400,
    .lcd_clock = 49434000,
    .h_active = 600,
    .v_active = 1024,
    .h_period = 770,
    .v_period = 1070,
    .hsync_width = 10,
    .hsync_bp = 80,
    .hsync_pol = 0,
    .vsync_width = 6,
    .vsync_bp = 20,
    .vsync_pol = 0,
};

constexpr PanelConfig kTv101wxmFtPanelConfig = {
    .name = "TV101WXM_FT",
    .dsi_on = lcd_init_sequence_TV101WXM_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kTv101wxmFitipowerDisplaySetting = {
    .lane_num = 4,
    .bit_rate_max = 566,
    .lcd_clock = 70701600,
    .h_active = 800,
    .v_active = 1280,
    .h_period = 890,
    .v_period = 1324,
    .hsync_width = 20,
    .hsync_bp = 50,
    .hsync_pol = 0,
    .vsync_width = 4,
    .vsync_bp = 20,
    .vsync_pol = 0,
};

constexpr PanelConfig kP101dezFtPanelConfig = {
    .name = "P101DEZ_FT",
    .dsi_on = lcd_init_sequence_P101DEZ_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kP101dezFitipowerDisplaySetting = {
    .lane_num = 4,
    .bit_rate_max = 566,
    .lcd_clock = 70701600,
    .h_active = 800,
    .v_active = 1280,
    .h_period = 890,
    .v_period = 1324,
    .hsync_width = 24,
    .hsync_bp = 20,
    .hsync_pol = 0,
    .vsync_width = 4,
    .vsync_bp = 20,
    .vsync_pol = 0,
};

constexpr PanelConfig kTv101wxmFt9365PanelConfig = {
    .name = "TV101WXM_FT_9365",
    .dsi_on = lcd_init_sequence_TV101WXM_FT_9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kTv070wsmFt9365PanelConfig = {
    .name = "TV070WSM_FT_9365",
    .dsi_on = lcd_init_sequence_TV070WSM_FT_9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kKd070d82FtPanelConfig = {
    .name = "KD070D82_FT",
    .dsi_on = lcd_init_sequence_KD070D82_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kKd070d82FitipowerDisplaySetting = {
    .lane_num = 4,
    .bit_rate_max = 400,
    .lcd_clock = 49434000,
    .h_active = 600,
    .v_active = 1024,
    .h_period = 770,
    .v_period = 1070,
    .hsync_width = 10,
    .hsync_bp = 80,
    .hsync_pol = 0,
    .vsync_width = 6,
    .vsync_bp = 20,
    .vsync_pol = 0,
};

constexpr PanelConfig kKd070d82Ft9365PanelConfig = {
    .name = "KD070D82_FT_9365",
    .dsi_on = lcd_init_sequence_KD070D82_FT_9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kVim3Ts050PanelConfig = {
    .name = "MTF050FHDI_03",
    .dsi_on = lcd_init_sequence_MTF050FHDI_03,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForVim3Ts050,
    .power_off = kLcdPowerOffSequenceForVim3Ts050,
};

// Display timing information of the MTF050FHDI-03 LCD used for Khadas TS050
// touchscreen. Provided by Microtech and tweaked by Khadas.
constexpr display_setting_t kMtf050fhdi03DisplaySetting = {
    .lane_num = 4,
    .bit_rate_max = 1000,
    .lcd_clock = 120'000'000,
    .h_active = 1080,
    .v_active = 1920,
    .h_period = 1120,
    .v_period = 1933,
    .hsync_width = 2,
    .hsync_bp = 23,
    .hsync_pol = 0,
    .vsync_width = 4,
    .vsync_bp = 7,
    .vsync_pol = 0,
};

constexpr PanelConfig kTv070wsmFtNelsonPanelConfig = {
    .name = "TV070WSM_FT_NELSON",
    .dsi_on = lcd_init_sequence_TV070WSM_FT_NELSON,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

}  // namespace

const PanelConfig* GetPanelConfig(uint32_t panel_type) {
  // LINT.IfChange
  switch (panel_type) {
    case PANEL_TV070WSM_FT_ASTRO:
      return &kTv070wsmFtAstroPanelConfig;
    case PANEL_P070ACB_FT:
      return &kP070acbFtPanelConfig;
    case PANEL_TV101WXM_FT:
      return &kTv101wxmFtPanelConfig;
    case PANEL_P101DEZ_FT:
      return &kP101dezFtPanelConfig;
    case PANEL_TV101WXM_FT_9365:
      return &kTv101wxmFt9365PanelConfig;
    case PANEL_TV070WSM_FT_9365:
      return &kTv070wsmFt9365PanelConfig;
    case PANEL_KD070D82_FT:
      return &kKd070d82FtPanelConfig;
    case PANEL_KD070D82_FT_9365:
      return &kKd070d82Ft9365PanelConfig;
    case PANEL_MTF050FHDI_03:
      return &kVim3Ts050PanelConfig;
    case PANEL_TV070WSM_FT_NELSON:
      return &kTv070wsmFtNelsonPanelConfig;
  }
  // LINT.ThenChange(//src/graphics/display/lib/device-protocol-display/include/lib/device-protocol/display-panel.h)
  return nullptr;
}

const display_setting_t* GetPanelDisplaySetting(uint32_t panel_type) {
  // LINT.IfChange
  switch (panel_type) {
    case PANEL_TV070WSM_FT_ASTRO:
    case PANEL_TV070WSM_FT_NELSON:
    case PANEL_TV070WSM_FT_9365:
      return &kTv070wsmFitipowerDisplaySetting;
    case PANEL_P070ACB_FT:
      return &kP070acbFitipowerDisplaySetting;
    case PANEL_KD070D82_FT_9365:
    case PANEL_KD070D82_FT:
      return &kKd070d82FitipowerDisplaySetting;
    case PANEL_TV101WXM_FT_9365:
    case PANEL_TV101WXM_FT:
      return &kTv101wxmFitipowerDisplaySetting;
    case PANEL_P101DEZ_FT:
      return &kP101dezFitipowerDisplaySetting;
    case PANEL_MTF050FHDI_03:
      return &kMtf050fhdi03DisplaySetting;
  }
  // LINT.ThenChange(//src/graphics/display/lib/device-protocol-display/include/lib/device-protocol/display-panel.h)
  return nullptr;
}

}  // namespace amlogic_display
