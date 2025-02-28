// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/designware-dsi/banjo-conversion.h"

#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <lib/mipi-dsi/mipi-dsi.h>
#include <zircon/assert.h>

#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/designware-dsi/dphy-interface-config.h"
#include "src/graphics/display/lib/designware-dsi/dpi-interface-config.h"
#include "src/graphics/display/lib/designware-dsi/dsi-host-controller-config.h"
#include "src/graphics/display/lib/designware-dsi/dsi-packet-handler-config.h"

namespace designware_dsi {

namespace {

constexpr void DebugAssertBanjoDisplaySettingIsValid(const display_setting_t& display_setting) {
  // The >= 0 assertions are always true for uint32_t members in the
  // `display_setting_t` struct and will be eventually optimized by the
  // compiler.
  //
  // These assertions, despite being always true, match the member
  // definitions in `DisplayTiming` and they make it easier for readers to
  // reason about the code without checking the types of each struct member.

  ZX_DEBUG_ASSERT(display_setting.lcd_clock >= 0);
  ZX_DEBUG_ASSERT(int64_t{display_setting.lcd_clock} <= display::kMaxPixelClockHz);

  ZX_DEBUG_ASSERT(display_setting.h_active >= 0);
  ZX_DEBUG_ASSERT(display_setting.h_active <= display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.h_period >= 0);
  ZX_DEBUG_ASSERT(display_setting.h_active <= display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.hsync_bp >= 0);
  ZX_DEBUG_ASSERT(display_setting.hsync_bp <= display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.hsync_width >= 0);
  ZX_DEBUG_ASSERT(display_setting.hsync_width <= display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.hsync_pol == 0 || display_setting.hsync_pol == 1);

  // `h_active`, `hsync_bp` and `hsync_width` are all within
  // [0..kMaxTimingValue], so adding these values won't cause an unsigned
  // overflow.
  ZX_DEBUG_ASSERT(display_setting.h_period >= display_setting.h_active + display_setting.hsync_bp +
                                                  display_setting.hsync_width);
  ZX_DEBUG_ASSERT(display_setting.h_period - (display_setting.h_active + display_setting.hsync_bp +
                                              display_setting.hsync_width) <=
                  display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.v_active >= 0);
  ZX_DEBUG_ASSERT(display_setting.v_active <= display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.v_period >= 0);
  ZX_DEBUG_ASSERT(display_setting.v_active <= display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.vsync_bp >= 0);
  ZX_DEBUG_ASSERT(display_setting.vsync_bp <= display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.vsync_width >= 0);
  ZX_DEBUG_ASSERT(display_setting.vsync_width <= display::kMaxTimingValue);

  // `v_active`, `vsync_bp` and `vsync_width` are all within
  // [0..kMaxTimingValue], so adding these values won't cause an unsigned
  // overflow.
  ZX_DEBUG_ASSERT(display_setting.v_period >= display_setting.v_active + display_setting.vsync_bp +
                                                  display_setting.vsync_width);
  ZX_DEBUG_ASSERT(display_setting.v_period - (display_setting.v_active + display_setting.vsync_bp +
                                              display_setting.vsync_width) <=
                  display::kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.vsync_pol == 0 || display_setting.vsync_pol == 1);
}

display::DisplayTiming ToDisplayTiming(const display_setting_t& banjo_display_setting) {
  DebugAssertBanjoDisplaySettingIsValid(banjo_display_setting);

  // A valid display_setting_t guarantees that `h_active`, `hsync_bp` and
  // `hsync_width` are all within [0..kMaxTimingValue],  so (h_active +
  // hsync_bp + hsync_width) won't overflow.
  //
  // It also guarantees that h_period >= (h_active + hsync_bp + hsync_width),
  // and h_period - (h_active + hsync_bp + hsync_width) is within
  // [0, kMaxTimingValue], so we can use int32_t to store its value.
  const int32_t horizontal_front_porch_px =
      static_cast<int32_t>(banjo_display_setting.h_period -
                           (banjo_display_setting.h_active + banjo_display_setting.hsync_bp +
                            banjo_display_setting.hsync_width));

  // Using an argument similar to the above, we can prove that the vertical
  // front porch value can be also stored in an int32_t.
  const int32_t vertical_front_porch_lines =
      static_cast<int32_t>(banjo_display_setting.v_period -
                           (banjo_display_setting.v_active + banjo_display_setting.vsync_bp +
                            banjo_display_setting.vsync_width));

  return display::DisplayTiming{
      .horizontal_active_px = static_cast<int32_t>(banjo_display_setting.h_active),
      .horizontal_front_porch_px = static_cast<int32_t>(horizontal_front_porch_px),
      .horizontal_sync_width_px = static_cast<int32_t>(banjo_display_setting.hsync_width),
      .horizontal_back_porch_px = static_cast<int32_t>(banjo_display_setting.hsync_bp),
      .vertical_active_lines = static_cast<int32_t>(banjo_display_setting.v_active),
      .vertical_front_porch_lines = vertical_front_porch_lines,
      .vertical_sync_width_lines = static_cast<int32_t>(banjo_display_setting.vsync_width),
      .vertical_back_porch_lines = static_cast<int32_t>(banjo_display_setting.vsync_bp),
      .pixel_clock_frequency_hz = banjo_display_setting.lcd_clock,
      .fields_per_frame = display::FieldsPerFrame::kProgressive,
      .hsync_polarity = (banjo_display_setting.hsync_pol == 1) ? display::SyncPolarity::kPositive
                                                               : display::SyncPolarity::kNegative,
      .vsync_polarity = (banjo_display_setting.vsync_pol == 1) ? display::SyncPolarity::kPositive
                                                               : display::SyncPolarity::kNegative,
      .vblank_alternates = false,
      .pixel_repetition = 0,
  };
}

}  // namespace

DpiColorComponentMapping ToDpiColorComponentMapping(color_code_t color_code) {
  switch (color_code) {
    case COLOR_CODE_PACKED_16BIT_565:
      // We cannot figure out the actual config using only the color_code.
      return DpiColorComponentMapping::k16BitR5G6B5Config1;
    case COLOR_CODE_PACKED_18BIT_666:
    case COLOR_CODE_LOOSE_24BIT_666:
      // We cannot figure out the actual config using only the color_code.
      return DpiColorComponentMapping::k18BitR6G6B6Config1;
    case COLOR_CODE_PACKED_24BIT_888:
      return DpiColorComponentMapping::k24BitR8G8B8;
  }
  ZX_DEBUG_ASSERT_MSG(false, "Invalid color code: %d", color_code);
  return DpiColorComponentMapping::k24BitR8G8B8;
}

mipi_dsi::DsiPixelStreamPacketFormat ToDsiPixelStreamPacketFormat(color_code_t color_code) {
  switch (color_code) {
    case COLOR_CODE_PACKED_16BIT_565:
      return mipi_dsi::DsiPixelStreamPacketFormat::k16BitR5G6B5;
    case COLOR_CODE_PACKED_18BIT_666:
      return mipi_dsi::DsiPixelStreamPacketFormat::k18BitR6G6B6;
    case COLOR_CODE_LOOSE_24BIT_666:
      return mipi_dsi::DsiPixelStreamPacketFormat::k18BitR6G6B6LooselyPacked;
    case COLOR_CODE_PACKED_24BIT_888:
      return mipi_dsi::DsiPixelStreamPacketFormat::k24BitR8G8B8;
  }
  ZX_DEBUG_ASSERT_MSG(false, "Invalid color code: %d", color_code);
  return mipi_dsi::DsiPixelStreamPacketFormat::k24BitR8G8B8;
}

mipi_dsi::DsiVideoModePacketSequencing ToDsiVideoModePacketSequencing(video_mode_t video_mode) {
  switch (video_mode) {
    case VIDEO_MODE_NON_BURST_PULSE:
      return mipi_dsi::DsiVideoModePacketSequencing::kSyncPulsesNoBurst;
    case VIDEO_MODE_NON_BURST_EVENT:
      return mipi_dsi::DsiVideoModePacketSequencing::kSyncEventsNoBurst;
    case VIDEO_MODE_BURST:
      return mipi_dsi::DsiVideoModePacketSequencing::kBurst;
  }
  ZX_DEBUG_ASSERT_MSG(false, "Invalid video mode: %d", video_mode);
  return mipi_dsi::DsiVideoModePacketSequencing::kBurst;
}

DsiHostControllerConfig ToDsiHostControllerConfig(const dsi_config_t& dsi_config,
                                                  int64_t dphy_data_lane_bits_per_second) {
  ZX_DEBUG_ASSERT(dsi_config.vendor_config_buffer != nullptr);
  ZX_DEBUG_ASSERT(dsi_config.vendor_config_size >= sizeof(designware_config_t));

  const designware_config_t& dw_config =
      *reinterpret_cast<designware_config_t*>(dsi_config.vendor_config_buffer);

  static constexpr int kDphyDataLaneBitsPerClockLaneCycle = 2;
  const int64_t high_speed_mode_clock_lane_frequency_hz =
      dphy_data_lane_bits_per_second / kDphyDataLaneBitsPerClockLaneCycle;
  static constexpr int kBitsPerByte = 8;
  const int64_t high_speed_mode_data_lane_bytes_per_second =
      dphy_data_lane_bits_per_second / kBitsPerByte;
  const int64_t escape_mode_clock_lane_frequency_hz =
      high_speed_mode_data_lane_bytes_per_second / dw_config.lp_escape_time;

  const DphyInterfaceConfig dphy_interface_config = {
      .data_lane_count = static_cast<int>(dsi_config.display_setting.lane_num),
      .clock_lane_mode_automatic_control_enabled = dw_config.auto_clklane != 0,

      .high_speed_mode_clock_lane_frequency_hz = high_speed_mode_clock_lane_frequency_hz,
      .escape_mode_clock_lane_frequency_hz = escape_mode_clock_lane_frequency_hz,

      .max_data_lane_hs_to_lp_transition_duration_lane_byte_clock_cycles =
          static_cast<int32_t>(dw_config.phy_timer_hs_to_lp),
      .max_data_lane_lp_to_hs_transition_duration_lane_byte_clock_cycles =
          static_cast<int32_t>(dw_config.phy_timer_lp_to_hs),
      .max_clock_lane_hs_to_lp_transition_duration_lane_byte_clock_cycles =
          static_cast<int32_t>(dw_config.phy_timer_clkhs_to_lp),
      .max_clock_lane_lp_to_hs_transition_duration_lane_byte_clock_cycles =
          static_cast<int32_t>(dw_config.phy_timer_clklp_to_hs),
  };

  const DpiColorComponentMapping color_component_mapping =
      ToDpiColorComponentMapping(dsi_config.color_coding);
  const display::DisplayTiming timing = ToDisplayTiming(dsi_config.display_setting);
  const DpiLowPowerCommandTimerConfig dpi_low_power_command_timer_config = {
      .max_vertical_blank_escape_mode_command_size_bytes =
          static_cast<int32_t>(dw_config.lp_cmd_pkt_size),
      .max_vertical_active_escape_mode_command_size_bytes =
          static_cast<int32_t>(dw_config.lp_cmd_pkt_size),
  };
  const DpiInterfaceConfig dpi_interface_config = {
      .color_component_mapping = color_component_mapping,
      .video_timing = timing,
      .low_power_command_timer_config = dpi_low_power_command_timer_config,
  };

  const DsiPacketHandlerConfig dsi_packet_handler_config = {
      .packet_sequencing = ToDsiVideoModePacketSequencing(dsi_config.video_mode_type),
      .pixel_stream_packet_format = ToDsiPixelStreamPacketFormat(dsi_config.color_coding),
  };

  const DsiHostControllerConfig dsi_host_controller_config = {
      .dphy_interface_config = dphy_interface_config,
      .dpi_interface_config = dpi_interface_config,
      .dsi_packet_handler_config = dsi_packet_handler_config,
  };
  return dsi_host_controller_config;
}

}  // namespace designware_dsi
