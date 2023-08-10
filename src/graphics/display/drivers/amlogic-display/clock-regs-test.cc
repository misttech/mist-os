// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"

#include <gtest/gtest.h>

namespace amlogic_display {

namespace {

TEST(VideoClock2Divider, GetSetDivider2) {
  auto reg = VideoClock2Divider::Get().FromValue(0);
  EXPECT_EQ(reg.SetDivider2(1).GetDivider2(), 1);
  EXPECT_EQ(reg.SetDivider2(2).GetDivider2(), 2);
  EXPECT_EQ(reg.SetDivider2(256).GetDivider2(), 256);
}

TEST(VideoClock1Divider, GetSetDivider1) {
  auto reg = VideoClock1Divider::Get().FromValue(0);
  EXPECT_EQ(reg.SetDivider1(1).GetDivider1(), 1);
  EXPECT_EQ(reg.SetDivider1(2).GetDivider1(), 2);
  EXPECT_EQ(reg.SetDivider1(256).GetDivider1(), 256);
}

TEST(VideoClock1Divider, GetSetDivider0) {
  auto reg = VideoClock1Divider::Get().FromValue(0);
  EXPECT_EQ(reg.SetDivider0(1).GetDivider0(), 1);
  EXPECT_EQ(reg.SetDivider0(2).GetDivider0(), 2);
  EXPECT_EQ(reg.SetDivider0(256).GetDivider0(), 256);
}

TEST(HdmiClockControl, GetSetHdmiSystemClockDivider) {
  auto reg = HdmiClockControl::Get().FromValue(0);
  EXPECT_EQ(reg.SetHdmiTxSystemClockDivider(1).GetHdmiTxSystemClockDivider(), 1);
  EXPECT_EQ(reg.SetHdmiTxSystemClockDivider(2).GetHdmiTxSystemClockDivider(), 2);
  EXPECT_EQ(reg.SetHdmiTxSystemClockDivider(128).GetHdmiTxSystemClockDivider(), 128);
}

}  // namespace

}  // namespace amlogic_display
