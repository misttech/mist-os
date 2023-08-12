// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"

#include <gtest/gtest.h>

namespace amlogic_display {

namespace {

TEST(VideoClock2Divider, GetSetDivider2) {
  auto reg = VideoClock2Divider::Get().FromValue(0);
  reg.SetDivider2(1);
  EXPECT_EQ(reg.Divider2(), 1);
  EXPECT_EQ(reg.divider2_minus_one(), 0u);
  reg.SetDivider2(2);
  EXPECT_EQ(reg.Divider2(), 2);
  EXPECT_EQ(reg.divider2_minus_one(), 1u);
  reg.SetDivider2(256);
  EXPECT_EQ(reg.Divider2(), 256);
  EXPECT_EQ(reg.divider2_minus_one(), 255u);
}

TEST(VideoClock1Divider, GetSetDivider1) {
  auto reg = VideoClock1Divider::Get().FromValue(0);
  reg.SetDivider1(1);
  EXPECT_EQ(reg.Divider1(), 1);
  EXPECT_EQ(reg.divider1_minus_one(), 0u);
  reg.SetDivider1(2);
  EXPECT_EQ(reg.Divider1(), 2);
  EXPECT_EQ(reg.divider1_minus_one(), 1u);
  reg.SetDivider1(256);
  EXPECT_EQ(reg.Divider1(), 256);
  EXPECT_EQ(reg.divider1_minus_one(), 255u);
}

TEST(VideoClock1Divider, GetSetDivider0) {
  auto reg = VideoClock1Divider::Get().FromValue(0);
  reg.SetDivider0(1);
  EXPECT_EQ(reg.Divider0(), 1);
  EXPECT_EQ(reg.divider0_minus_one(), 0u);
  reg.SetDivider0(2);
  EXPECT_EQ(reg.Divider0(), 2);
  EXPECT_EQ(reg.divider0_minus_one(), 1u);
  reg.SetDivider0(256);
  EXPECT_EQ(reg.Divider0(), 256);
  EXPECT_EQ(reg.divider0_minus_one(), 255u);
}

TEST(HdmiClockControl, GetSetHdmiSystemClockDivider) {
  auto reg = HdmiClockControl::Get().FromValue(0);
  reg.SetHdmiTxSystemClockDivider(1);
  EXPECT_EQ(reg.HdmiTxSystemClockDivider(), 1);
  EXPECT_EQ(reg.hdmi_tx_system_clock_divider_minus_one(), 0u);
  reg.SetHdmiTxSystemClockDivider(2);
  EXPECT_EQ(reg.HdmiTxSystemClockDivider(), 2);
  EXPECT_EQ(reg.hdmi_tx_system_clock_divider_minus_one(), 1u);
  reg.SetHdmiTxSystemClockDivider(128);
  EXPECT_EQ(reg.HdmiTxSystemClockDivider(), 128);
  EXPECT_EQ(reg.hdmi_tx_system_clock_divider_minus_one(), 127u);
}

TEST(VpuClockCControl, GetSetBranch1MuxDivider) {
  auto reg = VpuClockCControl::Get().FromValue(0);
  reg.SetBranch1MuxDivider(1);
  EXPECT_EQ(reg.Branch1MuxDivider(), 1);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 0u);
  reg.SetBranch1MuxDivider(2);
  EXPECT_EQ(reg.Branch1MuxDivider(), 2);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 1u);
  reg.SetBranch1MuxDivider(128);
  EXPECT_EQ(reg.Branch1MuxDivider(), 128);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 127u);
}

TEST(VpuClockCControl, GetSetBranch0MuxDivider) {
  auto reg = VpuClockCControl::Get().FromValue(0);
  reg.SetBranch0MuxDivider(1);
  EXPECT_EQ(reg.Branch0MuxDivider(), 1);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 0u);
  reg.SetBranch0MuxDivider(2);
  EXPECT_EQ(reg.Branch0MuxDivider(), 2);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 1u);
  reg.SetBranch0MuxDivider(128);
  EXPECT_EQ(reg.Branch0MuxDivider(), 128);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 127u);
}

TEST(VpuClockControl, GetSetBranch1MuxDivider) {
  auto reg = VpuClockControl::Get().FromValue(0);
  reg.SetBranch1MuxDivider(1);
  EXPECT_EQ(reg.Branch1MuxDivider(), 1);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 0u);
  reg.SetBranch1MuxDivider(2);
  EXPECT_EQ(reg.Branch1MuxDivider(), 2);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 1u);
  reg.SetBranch1MuxDivider(128);
  EXPECT_EQ(reg.Branch1MuxDivider(), 128);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 127u);
}

TEST(VpuClockControl, GetSetBranch0MuxDivider) {
  auto reg = VpuClockControl::Get().FromValue(0);
  reg.SetBranch0MuxDivider(1);
  EXPECT_EQ(reg.Branch0MuxDivider(), 1);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 0u);
  reg.SetBranch0MuxDivider(2);
  EXPECT_EQ(reg.Branch0MuxDivider(), 2);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 1u);
  reg.SetBranch0MuxDivider(128);
  EXPECT_EQ(reg.Branch0MuxDivider(), 128);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 127u);
}

TEST(VpuClockBControl, GetSetDivider1) {
  auto reg = VpuClockBControl::Get().FromValue(0);
  reg.SetDivider1(1);
  EXPECT_EQ(reg.Divider1(), 1);
  EXPECT_EQ(reg.divider1_minus_one(), 0u);
  reg.SetDivider1(2);
  EXPECT_EQ(reg.Divider1(), 2);
  EXPECT_EQ(reg.divider1_minus_one(), 1u);
  reg.SetDivider1(16);
  EXPECT_EQ(reg.Divider1(), 16);
  EXPECT_EQ(reg.divider1_minus_one(), 15u);
}

TEST(VpuClockBControl, GetSetDivider2) {
  auto reg = VpuClockBControl::Get().FromValue(0);
  reg.SetDivider2(1);
  EXPECT_EQ(reg.Divider2(), 1);
  EXPECT_EQ(reg.divider2_minus_one(), 0u);
  reg.SetDivider2(2);
  EXPECT_EQ(reg.Divider2(), 2);
  EXPECT_EQ(reg.divider2_minus_one(), 1u);
  reg.SetDivider2(256);
  EXPECT_EQ(reg.Divider2(), 256);
  EXPECT_EQ(reg.divider2_minus_one(), 255u);
}

TEST(VideoAdvancedPeripheralBusClockControl, GetSetBranch1MuxDivider) {
  auto reg = VideoAdvancedPeripheralBusClockControl::Get().FromValue(0);
  reg.SetBranch1MuxDivider(1);
  EXPECT_EQ(reg.Branch1MuxDivider(), 1);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 0u);
  reg.SetBranch1MuxDivider(2);
  EXPECT_EQ(reg.Branch1MuxDivider(), 2);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 1u);
  reg.SetBranch1MuxDivider(128);
  EXPECT_EQ(reg.Branch1MuxDivider(), 128);
  EXPECT_EQ(reg.branch1_mux_divider_minus_one(), 127u);
}

TEST(VideoAdvancedPeripheralBusClockControl, GetSetBranch0MuxDivider) {
  auto reg = VideoAdvancedPeripheralBusClockControl::Get().FromValue(0);
  reg.SetBranch0MuxDivider(1);
  EXPECT_EQ(reg.Branch0MuxDivider(), 1);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 0u);
  reg.SetBranch0MuxDivider(2);
  EXPECT_EQ(reg.Branch0MuxDivider(), 2);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 1u);
  reg.SetBranch0MuxDivider(128);
  EXPECT_EQ(reg.Branch0MuxDivider(), 128);
  EXPECT_EQ(reg.branch0_mux_divider_minus_one(), 127u);
}

TEST(VideoInputMeasureClockControl, GetSetDsiDivider) {
  auto reg = VideoInputMeasureClockControl::Get().FromValue(0);
  reg.SetDsiMeasureClockDivider(1);
  EXPECT_EQ(reg.DsiMeasureClockDivider(), 1);
  EXPECT_EQ(reg.dsi_measure_clock_divider_minus_one(), 0u);
  reg.SetDsiMeasureClockDivider(2);
  EXPECT_EQ(reg.DsiMeasureClockDivider(), 2);
  EXPECT_EQ(reg.dsi_measure_clock_divider_minus_one(), 1u);
  reg.SetDsiMeasureClockDivider(128);
  EXPECT_EQ(reg.DsiMeasureClockDivider(), 128);
  EXPECT_EQ(reg.dsi_measure_clock_divider_minus_one(), 127u);
}

TEST(VideoInputMeasureClockControl, GetSetVideoInputDivider) {
  auto reg = VideoInputMeasureClockControl::Get().FromValue(0);
  reg.SetVideoInputMeasureClockDivider(1);
  EXPECT_EQ(reg.VideoInputMeasureClockDivider(), 1);
  EXPECT_EQ(reg.video_input_measure_clock_divider_minus_one(), 0u);
  reg.SetVideoInputMeasureClockDivider(2);
  EXPECT_EQ(reg.VideoInputMeasureClockDivider(), 2);
  EXPECT_EQ(reg.video_input_measure_clock_divider_minus_one(), 1u);
  reg.SetVideoInputMeasureClockDivider(128);
  EXPECT_EQ(reg.VideoInputMeasureClockDivider(), 128);
  EXPECT_EQ(reg.video_input_measure_clock_divider_minus_one(), 127u);
}

TEST(MipiDsiPhyClockControl, GetSetDivider) {
  auto reg = MipiDsiPhyClockControl::Get().FromValue(0);
  reg.SetDivider(1);
  EXPECT_EQ(reg.Divider(), 1);
  EXPECT_EQ(reg.divider_minus_one(), 0u);
  reg.SetDivider(2);
  EXPECT_EQ(reg.Divider(), 2);
  EXPECT_EQ(reg.divider_minus_one(), 1u);
  reg.SetDivider(128);
  EXPECT_EQ(reg.Divider(), 128);
  EXPECT_EQ(reg.divider_minus_one(), 127u);
}

}  // namespace

}  // namespace amlogic_display
