// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/alpha-mode.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr AlphaMode kPremultiplied2(
    fuchsia_hardware_display_types::wire::AlphaMode::kPremultiplied);

TEST(AlphaModeTest, EqualityIsReflexive) {
  EXPECT_EQ(AlphaMode::kPremultiplied, AlphaMode::kPremultiplied);
  EXPECT_EQ(kPremultiplied2, kPremultiplied2);
  EXPECT_EQ(AlphaMode::kDisable, AlphaMode::kDisable);
}

TEST(AlphaModeTest, EqualityIsSymmetric) {
  EXPECT_EQ(AlphaMode::kPremultiplied, kPremultiplied2);
  EXPECT_EQ(kPremultiplied2, AlphaMode::kPremultiplied);
}

TEST(AlphaModeTest, EqualityForDifferentValues) {
  EXPECT_NE(AlphaMode::kPremultiplied, AlphaMode::kDisable);
  EXPECT_NE(AlphaMode::kDisable, AlphaMode::kPremultiplied);
  EXPECT_NE(kPremultiplied2, AlphaMode::kDisable);
  EXPECT_NE(AlphaMode::kDisable, kPremultiplied2);
}

TEST(AlphaModeTest, ToBanjoAlphaMode) {
  static constexpr coordinate_transformation_t banjo_transformation =
      AlphaMode::kPremultiplied.ToBanjo();
  EXPECT_EQ(ALPHA_PREMULTIPLIED, banjo_transformation);
}

TEST(AlphaModeTest, ToFidlAlphaMode) {
  static constexpr fuchsia_hardware_display_types::wire::AlphaMode fidl_transformation =
      AlphaMode::kPremultiplied.ToFidl();
  EXPECT_EQ(fuchsia_hardware_display_types::wire::AlphaMode::kPremultiplied, fidl_transformation);
}

TEST(AlphaModeTest, ToAlphaModeWithBanjoValue) {
  static constexpr AlphaMode transformation(ALPHA_PREMULTIPLIED);
  EXPECT_EQ(AlphaMode::kPremultiplied, transformation);
}

TEST(AlphaModeTest, ToAlphaModeWithFidlValue) {
  static constexpr AlphaMode transformation(
      fuchsia_hardware_display_types::wire::AlphaMode::kPremultiplied);
  EXPECT_EQ(AlphaMode::kPremultiplied, transformation);
}

TEST(AlphaModeTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(AlphaMode::kPremultiplied, AlphaMode(AlphaMode::kPremultiplied.ToBanjo()));
  EXPECT_EQ(AlphaMode::kDisable, AlphaMode(AlphaMode::kDisable.ToBanjo()));
}

TEST(AlphaModeTest, FidlConversionRoundtrip) {
  EXPECT_EQ(AlphaMode::kPremultiplied, AlphaMode(AlphaMode::kPremultiplied.ToFidl()));
  EXPECT_EQ(AlphaMode::kDisable, AlphaMode(AlphaMode::kDisable.ToFidl()));
}

}  // namespace

}  // namespace display
