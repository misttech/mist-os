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

constexpr PanelConfig kTv070wsmFtPanelConfig = {
    .name = "TV070WSM_FT",
    .dsi_on = lcd_init_sequence_TV070WSM_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kP070acbFtPanelConfig = {
    .name = "P070ACB_FT",
    .dsi_on = lcd_init_sequence_P070ACB_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kTv101wxmFtPanelConfig = {
    .name = "TV101WXM_FT",
    .dsi_on = lcd_init_sequence_TV101WXM_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kG101b158FtPanelConfig = {
    .name = "G101B158_FT",
    .dsi_on = lcd_init_sequence_G101B158_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kTv080wxmFtPanelConfig = {
    .name = "TV080WXM_FT",
    .dsi_on = lcd_init_sequence_TV080WXM_FT,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
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

constexpr PanelConfig kKd070d82Ft9365PanelConfig = {
    .name = "KD070D82_FT_9365",
    .dsi_on = lcd_init_sequence_KD070D82_FT_9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,
};

constexpr PanelConfig kTv070wsmSt7703iPanelConfig = {
    .name = "TV070WSM_ST7703I",
    .dsi_on = lcd_init_sequence_TV070WSM_ST7703I,
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

}  // namespace

const PanelConfig* GetPanelConfig(uint32_t panel_type) {
  // LINT.IfChange
  switch (panel_type) {
    case PANEL_TV070WSM_FT:
      return &kTv070wsmFtPanelConfig;
    case PANEL_P070ACB_FT:
      return &kP070acbFtPanelConfig;
    case PANEL_TV101WXM_FT:
      return &kTv101wxmFtPanelConfig;
    case PANEL_G101B158_FT:
      return &kG101b158FtPanelConfig;
    case PANEL_TV080WXM_FT:
      return &kTv080wxmFtPanelConfig;
    case PANEL_TV101WXM_FT_9365:
      return &kTv101wxmFt9365PanelConfig;
    case PANEL_TV070WSM_FT_9365:
      return &kTv070wsmFt9365PanelConfig;
    case PANEL_KD070D82_FT:
      return &kKd070d82FtPanelConfig;
    case PANEL_KD070D82_FT_9365:
      return &kKd070d82Ft9365PanelConfig;
    case PANEL_TV070WSM_ST7703I:
      return &kTv070wsmSt7703iPanelConfig;
    case PANEL_MTF050FHDI_03:
      return &kVim3Ts050PanelConfig;
  }
  // LINT.ThenChange(//src/graphics/display/lib/device-protocol-display/include/lib/device-protocol/display-panel.h)
  return nullptr;
}

}  // namespace amlogic_display
