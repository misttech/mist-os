// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/alpha-mode.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(AlphaMode, FidlConversion) {
  EXPECT_EQ(ToAlphaMode(fuchsia_hardware_display::wire::AlphaMode::kDisable), AlphaMode::kDisable);
  EXPECT_EQ(ToAlphaMode(fuchsia_hardware_display::wire::AlphaMode::kPremultiplied),
            AlphaMode::kPremultiplied);
  EXPECT_EQ(ToAlphaMode(fuchsia_hardware_display::wire::AlphaMode::kHwMultiply),
            AlphaMode::kHwMultiply);

  EXPECT_EQ(ToFidlAlphaMode(AlphaMode::kDisable),
            fuchsia_hardware_display::wire::AlphaMode::kDisable);
  EXPECT_EQ(ToFidlAlphaMode(AlphaMode::kPremultiplied),
            fuchsia_hardware_display::wire::AlphaMode::kPremultiplied);
  EXPECT_EQ(ToFidlAlphaMode(AlphaMode::kHwMultiply),
            fuchsia_hardware_display::wire::AlphaMode::kHwMultiply);
}

TEST(AlphaMode, BanjoConversion) {
  EXPECT_EQ(ToAlphaMode(ALPHA_DISABLE), AlphaMode::kDisable);
  EXPECT_EQ(ToAlphaMode(ALPHA_PREMULTIPLIED), AlphaMode::kPremultiplied);
  EXPECT_EQ(ToAlphaMode(ALPHA_HW_MULTIPLY), AlphaMode::kHwMultiply);

  EXPECT_EQ(ToBanjoAlpha(AlphaMode::kDisable), ALPHA_DISABLE);
  EXPECT_EQ(ToBanjoAlpha(AlphaMode::kPremultiplied), ALPHA_PREMULTIPLIED);
  EXPECT_EQ(ToBanjoAlpha(AlphaMode::kHwMultiply), ALPHA_HW_MULTIPLY);
}

}  // namespace

}  // namespace display
