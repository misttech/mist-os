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

constexpr PanelConfig kBoeTv070wsmFitipowerJd9364AstroPanelConfig = {
    .name = "BOE_TV070WSM_FITIPOWER_JD9364_ASTRO",
    .dsi_on = lcd_init_sequence_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kBoeTv070wsmFitipowerDisplaySetting = {
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

constexpr PanelConfig kInnoluxP070acbFitipowerJd9364PanelConfig = {
    .name = "INNOLUX_P070ACB_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_INNOLUX_P070ACB_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kInnoluxP070acbFitipowerDisplaySetting = {
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

constexpr PanelConfig kBoeTv101wxmFitipowerJd9364PanelConfig = {
    .name = "BOE_TV101WXM_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_BOE_TV101WXM_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kBoeTv101wxmFitipowerDisplaySetting = {
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

constexpr PanelConfig kInnoluxP101dezFitipowerJd9364PanelConfig = {
    .name = "INNOLUX_P101DEZ_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_INNOLUX_P101DEZ_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kInnoluxP101dezFitipowerDisplaySetting = {
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

constexpr PanelConfig kBoeTv101wxmFitipowerJd9365PanelConfig = {
    .name = "BOE_TV101WXM_FITIPOWER_JD9365",
    .dsi_on = lcd_init_sequence_BOE_TV101WXM_FITIPOWER_JD9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kBoeTv070wsmFitipowerJd9365PanelConfig = {
    .name = "BOE_TV070WSM_FITIPOWER_JD9365",
    .dsi_on = lcd_init_sequence_BOE_TV070WSM_FITIPOWER_JD9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kKdKd070d82FitipowerJd9364PanelConfig = {
    .name = "KD_KD070D82_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_KD_KD070D82_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr display_setting_t kKdKd070d82FitipowerDisplaySetting = {
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

constexpr PanelConfig kKdKd070d82FitipowerJd9365PanelConfig = {
    .name = "KD_KD070D82_FITIPOWER_JD9365",
    .dsi_on = lcd_init_sequence_KD_KD070D82_FITIPOWER_JD9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kMicrotechMtf050fhdi03NovatekNt35596PanelConfig = {
    .name = "MICROTECH_MTF050FHDI03_NOVATEK_NT35596",
    .dsi_on = lcd_init_sequence_MICROTECH_MTF050FHDI03_NOVATEK_NT35596,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForVim3Ts050,
    .power_off = kLcdPowerOffSequenceForVim3Ts050,
};

// Display timing information of the MTF050FHDI-03 LCD used for Khadas TS050
// touchscreen. Provided by Microtech and tweaked by Khadas.
constexpr display_setting_t kMicrotechMtf050fhdi03NovatekNt35596DisplaySetting = {
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

constexpr PanelConfig kBoeTv070wsmFitipowerJd9364NelsonPanelConfig = {
    .name = "BOE_TV070WSM_FITIPOWER_JD9364_NELSON",
    .dsi_on = lcd_init_sequence_BOE_TV070WSM_FITIPOWER_JD9364_NELSON,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

}  // namespace

const PanelConfig* GetPanelConfig(uint32_t panel_type) {
  // LINT.IfChange
  switch (panel_type) {
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO:
      return &kBoeTv070wsmFitipowerJd9364AstroPanelConfig;
    case PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364:
      return &kInnoluxP070acbFitipowerJd9364PanelConfig;
    case PANEL_BOE_TV101WXM_FITIPOWER_JD9364:
      return &kBoeTv101wxmFitipowerJd9364PanelConfig;
    case PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364:
      return &kInnoluxP101dezFitipowerJd9364PanelConfig;
    case PANEL_BOE_TV101WXM_FITIPOWER_JD9365:
      return &kBoeTv101wxmFitipowerJd9365PanelConfig;
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9365:
      return &kBoeTv070wsmFitipowerJd9365PanelConfig;
    case PANEL_KD_KD070D82_FITIPOWER_JD9364:
      return &kKdKd070d82FitipowerJd9364PanelConfig;
    case PANEL_KD_KD070D82_FITIPOWER_JD9365:
      return &kKdKd070d82FitipowerJd9365PanelConfig;
    case PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596:
      return &kMicrotechMtf050fhdi03NovatekNt35596PanelConfig;
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON:
      return &kBoeTv070wsmFitipowerJd9364NelsonPanelConfig;
  }
  // LINT.ThenChange(//src/graphics/display/lib/device-protocol-display/include/lib/device-protocol/display-panel.h)
  return nullptr;
}

const display_setting_t* GetPanelDisplaySetting(uint32_t panel_type) {
  // LINT.IfChange
  switch (panel_type) {
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO:
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON:
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9365:
      return &kBoeTv070wsmFitipowerDisplaySetting;
    case PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364:
      return &kInnoluxP070acbFitipowerDisplaySetting;
    case PANEL_KD_KD070D82_FITIPOWER_JD9365:
    case PANEL_KD_KD070D82_FITIPOWER_JD9364:
      return &kKdKd070d82FitipowerDisplaySetting;
    case PANEL_BOE_TV101WXM_FITIPOWER_JD9365:
    case PANEL_BOE_TV101WXM_FITIPOWER_JD9364:
      return &kBoeTv101wxmFitipowerDisplaySetting;
    case PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364:
      return &kInnoluxP101dezFitipowerDisplaySetting;
    case PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596:
      return &kMicrotechMtf050fhdi03NovatekNt35596DisplaySetting;
  }
  // LINT.ThenChange(//src/graphics/display/lib/device-protocol-display/include/lib/device-protocol/display-panel.h)
  return nullptr;
}

}  // namespace amlogic_display
