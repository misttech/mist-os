// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/designware/color-param.h"

#include <fidl/fuchsia.hardware.hdmi/cpp/wire.h>

#include <gtest/gtest.h>

namespace hdmi_dw {

namespace {

TEST(ColorFormat, FidlConversion) {
  EXPECT_EQ(ToColorFormat(fuchsia_hardware_hdmi::ColorFormat::kCfRgb), ColorFormat::kCfRgb);
  EXPECT_EQ(ToColorFormat(fuchsia_hardware_hdmi::ColorFormat::kCf444), ColorFormat::kCf444);
}

TEST(ColorDepth, FidlConversion) {
  EXPECT_EQ(ToColorDepth(fuchsia_hardware_hdmi::ColorDepth::kCd24B), ColorDepth::kCd24B);
  EXPECT_EQ(ToColorDepth(fuchsia_hardware_hdmi::ColorDepth::kCd30B), ColorDepth::kCd30B);
  EXPECT_EQ(ToColorDepth(fuchsia_hardware_hdmi::ColorDepth::kCd36B), ColorDepth::kCd36B);
  EXPECT_EQ(ToColorDepth(fuchsia_hardware_hdmi::ColorDepth::kCd48B), ColorDepth::kCd48B);
}

TEST(ColorParam, FidlConversion) {
  constexpr fuchsia_hardware_hdmi::wire::ColorParam kFidl = {
      .input_color_format = fuchsia_hardware_hdmi::ColorFormat::kCfRgb,
      .output_color_format = fuchsia_hardware_hdmi::ColorFormat::kCf444,
      .color_depth = fuchsia_hardware_hdmi::ColorDepth::kCd24B,
  };
  constexpr ColorParam actual_color_param = ToColorParam(kFidl);
  EXPECT_EQ(actual_color_param.input_color_format, ColorFormat::kCfRgb);
  EXPECT_EQ(actual_color_param.output_color_format, ColorFormat::kCf444);
  EXPECT_EQ(actual_color_param.color_depth, ColorDepth::kCd24B);
}

}  // namespace

}  // namespace hdmi_dw
