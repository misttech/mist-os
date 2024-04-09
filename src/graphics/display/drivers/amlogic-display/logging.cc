// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/logging.h"

#include <cinttypes>

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/logging/zxlogf.h"

namespace amlogic_display {

void LogDisplayTiming(const display::DisplayTiming& display_timing) {
  zxlogf(INFO, "Display Timing: ");
  zxlogf(INFO, "  Horizontal active (px): %" PRId32, display_timing.horizontal_active_px);
  zxlogf(INFO, "  Horizontal front porch (px): %" PRId32, display_timing.horizontal_front_porch_px);
  zxlogf(INFO, "  Horizontal sync width (px): %" PRId32, display_timing.horizontal_sync_width_px);
  zxlogf(INFO, "  Horizontal back porch (px): %" PRId32, display_timing.horizontal_back_porch_px);
  zxlogf(INFO, "  Horizontal blank (px): %" PRId32, display_timing.horizontal_blank_px());
  zxlogf(INFO, "  Horizontal total (px): %" PRId32, display_timing.horizontal_total_px());
  zxlogf(INFO, "");
  zxlogf(INFO, "  Vertical active (lines): %" PRId32, display_timing.vertical_active_lines);
  zxlogf(INFO, "  Vertical front porch (lines): %" PRId32,
         display_timing.vertical_front_porch_lines);
  zxlogf(INFO, "  Vertical sync width (lines): %" PRId32, display_timing.vertical_sync_width_lines);
  zxlogf(INFO, "  Vertical back porch (lines): %" PRId32, display_timing.vertical_back_porch_lines);
  zxlogf(INFO, "  Vertical blank (lines): %" PRId32, display_timing.vertical_blank_lines());
  zxlogf(INFO, "  Vertical total (lines): %" PRId32, display_timing.vertical_total_lines());
  zxlogf(INFO, "");
  zxlogf(INFO, "  Pixel clock frequency (Hz): %" PRId64, display_timing.pixel_clock_frequency_hz);
  zxlogf(INFO, "  Fields per frame: %s",
         display_timing.fields_per_frame == display::FieldsPerFrame::kInterlaced ? "Interlaced"
                                                                                 : "Progressive");
  zxlogf(
      INFO, "  Hsync polarity: %s",
      display_timing.hsync_polarity == display::SyncPolarity::kPositive ? "Positive" : "Negative");
  zxlogf(
      INFO, "  Vsync polarity: %s",
      display_timing.vsync_polarity == display::SyncPolarity::kPositive ? "Positive" : "Negative");
  zxlogf(INFO, "  Vblank alternates: %s", display_timing.vblank_alternates ? "True" : "False");
  zxlogf(INFO, "  Pixel repetition: %" PRId32, display_timing.pixel_repetition);
}

void LogPanelConfig(const PanelConfig& panel_config) {
  zxlogf(INFO, "Panel Config for Panel \"%s\"", panel_config.name);
  zxlogf(INFO, "  Power on DSI command sequence: size %zu", panel_config.dsi_on.size());
  zxlogf(INFO, "  Power off DSI command sequence: size %zu", panel_config.dsi_off.size());
  zxlogf(INFO, "  Power on PowerOp sequence: size %zu", panel_config.power_on.size());
  zxlogf(INFO, "  Power off PowerOp sequence: size %zu", panel_config.power_off.size());
  zxlogf(INFO, "");
  zxlogf(INFO, "  D-PHY data lane count: %" PRId32, panel_config.dphy_data_lane_count);
  zxlogf(INFO, "  Maximum D-PHY clock lane frequency (Hz): %" PRId64,
         panel_config.maximum_dphy_clock_lane_frequency_hz);
  zxlogf(INFO, "  Maximum D-PHY data lane bitrate (bit/second): %" PRId64,
         panel_config.maximum_per_data_lane_bit_per_second());
  zxlogf(INFO, "");
  LogDisplayTiming(panel_config.display_timing);
}

}  // namespace amlogic_display
