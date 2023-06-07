// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/frame.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(Frame, FidlConversion) {
  fuchsia_hardware_display::wire::Frame fidl_from = {
      .x_pos = 1,
      .y_pos = 2,
      .width = 3,
      .height = 4,
  };

  Frame frame = ToFrame(fidl_from);
  EXPECT_EQ(frame.x_pos, 1);
  EXPECT_EQ(frame.y_pos, 2);
  EXPECT_EQ(frame.width, 3);
  EXPECT_EQ(frame.height, 4);

  fuchsia_hardware_display::wire::Frame fidl_to = ToFidlFrame(frame);
  EXPECT_EQ(fidl_to.x_pos, 1u);
  EXPECT_EQ(fidl_to.y_pos, 2u);
  EXPECT_EQ(fidl_to.width, 3u);
  EXPECT_EQ(fidl_to.height, 4u);
}

TEST(Frame, BanjoConversion) {
  frame_t banjo_from = {
      .x_pos = 1,
      .y_pos = 2,
      .width = 3,
      .height = 4,
  };

  Frame frame = ToFrame(banjo_from);
  EXPECT_EQ(frame.x_pos, 1);
  EXPECT_EQ(frame.y_pos, 2);
  EXPECT_EQ(frame.width, 3);
  EXPECT_EQ(frame.height, 4);

  frame_t banjo_to = ToBanjoFrame(frame);
  EXPECT_EQ(banjo_to.x_pos, 1u);
  EXPECT_EQ(banjo_to.y_pos, 2u);
  EXPECT_EQ(banjo_to.width, 3u);
  EXPECT_EQ(banjo_to.height, 4u);
}

}  // namespace

}  // namespace display
