// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/designware-dsi/banjo-conversion.h"

#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <lib/mipi-dsi/mipi-dsi.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/designware-dsi/dphy-interface-config.h"
#include "src/graphics/display/lib/designware-dsi/dpi-interface-config.h"
#include "src/graphics/display/lib/designware-dsi/dsi-host-controller-config.h"
#include "src/graphics/display/lib/designware-dsi/dsi-packet-handler-config.h"

namespace designware_dsi {

namespace {

TEST(BanjoConversionTest, ToDpiColorComponentMapping) {
  EXPECT_EQ(ToDpiColorComponentMapping(COLOR_CODE_PACKED_16BIT_565),
            DpiColorComponentMapping::k16BitR5G6B5Config1);
  EXPECT_EQ(ToDpiColorComponentMapping(COLOR_CODE_PACKED_18BIT_666),
            DpiColorComponentMapping::k18BitR6G6B6Config1);
  EXPECT_EQ(ToDpiColorComponentMapping(COLOR_CODE_LOOSE_24BIT_666),
            DpiColorComponentMapping::k18BitR6G6B6Config1);
  EXPECT_EQ(ToDpiColorComponentMapping(COLOR_CODE_PACKED_24BIT_888),
            DpiColorComponentMapping::k24BitR8G8B8);
}

TEST(BanjoConversionTest, ToDsiPixelStreamPacketFormat) {
  EXPECT_EQ(ToDsiPixelStreamPacketFormat(COLOR_CODE_PACKED_16BIT_565),
            mipi_dsi::DsiPixelStreamPacketFormat::k16BitR5G6B5);
  EXPECT_EQ(ToDsiPixelStreamPacketFormat(COLOR_CODE_PACKED_18BIT_666),
            mipi_dsi::DsiPixelStreamPacketFormat::k18BitR6G6B6);
  EXPECT_EQ(ToDsiPixelStreamPacketFormat(COLOR_CODE_LOOSE_24BIT_666),
            mipi_dsi::DsiPixelStreamPacketFormat::k18BitR6G6B6LooselyPacked);
  EXPECT_EQ(ToDsiPixelStreamPacketFormat(COLOR_CODE_PACKED_24BIT_888),
            mipi_dsi::DsiPixelStreamPacketFormat::k24BitR8G8B8);
}

TEST(BanjoConversionTest, ToDsiHostControllerConfig) {
  designware_config_t dw_cfg = {
      .lp_escape_time = 10,
      .lp_cmd_pkt_size = 0xab,
      .phy_timer_clkhs_to_lp = 0x1aa,
      .phy_timer_clklp_to_hs = 0x1bb,
      .phy_timer_hs_to_lp = 0x1cc,
      .phy_timer_lp_to_hs = 0x1dd,
      .auto_clklane = true,
  };

  const display_setting_t display_setting = {
      .lane_num = 4,
      .bit_rate_max = 600'000'000,
      .lcd_clock = 10'000'000,
      .h_active = 0x333,
      .v_active = 0x222,
      .h_period = 0x3ff,
      .v_period = 0x2ff,
      .hsync_width = 0x11,
      .hsync_bp = 0x22,
      .hsync_pol = true,
      .vsync_width = 0x33,
      .vsync_bp = 0x44,
      .vsync_pol = false,
  };

  const dsi_config_t dsi_config = {
      .display_setting = display_setting,
      .video_mode_type = VIDEO_MODE_BURST,
      .color_coding = COLOR_CODE_PACKED_24BIT_888,
      .vendor_config_buffer = reinterpret_cast<uint8_t*>(&dw_cfg),
      .vendor_config_size = sizeof(dw_cfg),
  };

  static constexpr int64_t kHighSpeedDataLaneBitsPerSecond = 400'000'000;
  // clock lane frequency = data lane bit rate (bit/s) / 2
  static constexpr int64_t kHighSpeedClockLaneFrequencyHz = 200'000'000;
  // data lane byte rate (byte/s) = data lane bit rate (bit/s) / 8
  static constexpr int64_t kHighSpeedDataLaneBytesPerSecond = 50'000'000;
  // escape clock lane frequency = data lane byte rate (byte/s) / lp_escape_time
  static constexpr int64_t kEscapeClockLaneFrequencyHz = 5'000'000;
  // data lane bit rate (bit/s) = clock lane frequency * 2
  static constexpr int64_t kEscapeDataLaneBitsPerSecond = 10'000'000;
  // data lane byte rate (byte/s) = data lane bit rate (bit/s) / 8
  static constexpr int64_t kEscapeDataLaneBytesPerSecond = 1'250'000;

  const DsiHostControllerConfig dsi_host_controller_config =
      ToDsiHostControllerConfig(dsi_config, kHighSpeedDataLaneBitsPerSecond);

  const DphyInterfaceConfig& dphy_interface_config =
      dsi_host_controller_config.dphy_interface_config;
  EXPECT_EQ(dphy_interface_config.data_lane_count, 4);
  EXPECT_EQ(dphy_interface_config.clock_lane_mode_automatic_control_enabled, true);
  EXPECT_EQ(dphy_interface_config.high_speed_mode_clock_lane_frequency_hz,
            kHighSpeedClockLaneFrequencyHz);
  EXPECT_EQ(dphy_interface_config.high_speed_mode_data_lane_bits_per_second(),
            kHighSpeedDataLaneBitsPerSecond);
  EXPECT_EQ(dphy_interface_config.high_speed_mode_data_lane_bytes_per_second(),
            kHighSpeedDataLaneBytesPerSecond);
  EXPECT_EQ(dphy_interface_config.escape_mode_clock_lane_frequency_hz, kEscapeClockLaneFrequencyHz);
  EXPECT_EQ(dphy_interface_config.escape_mode_data_lane_bits_per_second(),
            kEscapeDataLaneBitsPerSecond);
  EXPECT_EQ(dphy_interface_config.escape_mode_data_lane_bytes_per_second(),
            kEscapeDataLaneBytesPerSecond);
  EXPECT_EQ(dphy_interface_config.max_data_lane_hs_to_lp_transition_duration_lane_byte_clock_cycles,
            0x1cc);
  EXPECT_EQ(dphy_interface_config.max_data_lane_lp_to_hs_transition_duration_lane_byte_clock_cycles,
            0x1dd);
  EXPECT_EQ(
      dphy_interface_config.max_clock_lane_hs_to_lp_transition_duration_lane_byte_clock_cycles,
      0x1aa);
  EXPECT_EQ(
      dphy_interface_config.max_clock_lane_lp_to_hs_transition_duration_lane_byte_clock_cycles,
      0x1bb);

  const DpiInterfaceConfig& dpi_interface_config = dsi_host_controller_config.dpi_interface_config;
  EXPECT_EQ(dpi_interface_config.color_component_mapping, DpiColorComponentMapping::k24BitR8G8B8);
  EXPECT_EQ(dpi_interface_config.low_power_command_timer_config
                .max_vertical_active_escape_mode_command_size_bytes,
            0xab);
  EXPECT_EQ(dpi_interface_config.low_power_command_timer_config
                .max_vertical_blank_escape_mode_command_size_bytes,
            0xab);

  const display::DisplayTiming& timing = dpi_interface_config.video_timing;
  EXPECT_EQ(timing.horizontal_active_px, 0x333);
  EXPECT_EQ(timing.vertical_active_lines, 0x222);
  EXPECT_EQ(timing.horizontal_total_px(), 0x3ff);
  EXPECT_EQ(timing.vertical_total_lines(), 0x2ff);
  EXPECT_EQ(timing.horizontal_sync_width_px, 0x11);
  EXPECT_EQ(timing.horizontal_back_porch_px, 0x22);
  EXPECT_EQ(timing.hsync_polarity, display::SyncPolarity::kPositive);
  EXPECT_EQ(timing.vertical_sync_width_lines, 0x33);
  EXPECT_EQ(timing.vertical_back_porch_lines, 0x44);
  EXPECT_EQ(timing.vsync_polarity, display::SyncPolarity::kNegative);

  const DsiPacketHandlerConfig& dsi_packet_handler_config =
      dsi_host_controller_config.dsi_packet_handler_config;
  EXPECT_EQ(dsi_packet_handler_config.packet_sequencing,
            mipi_dsi::DsiVideoModePacketSequencing::kBurst);
  EXPECT_EQ(dsi_packet_handler_config.pixel_stream_packet_format,
            mipi_dsi::DsiPixelStreamPacketFormat::k24BitR8G8B8);
}

}  // namespace

}  // namespace designware_dsi
