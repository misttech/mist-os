// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/dimensions.h"

#include <fidl/fuchsia.math/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr Dimensions kSmall({
    .width = 10,
    .height = 20,
});

constexpr Dimensions kSmall2({
    .width = 10,
    .height = 20,
});

constexpr Dimensions kMedium({
    .width = 10000,
    .height = 20000,
});

TEST(DimensionsTest, EqualityIsReflexive) {
  EXPECT_EQ(kSmall, kSmall);
  EXPECT_EQ(kSmall2, kSmall2);
  EXPECT_EQ(kMedium, kMedium);
}

TEST(DimensionsTest, EqualityIsSymmetric) {
  EXPECT_EQ(kSmall, kSmall2);
  EXPECT_EQ(kSmall2, kSmall);
}

TEST(DimensionsTest, EqualityForDifferentWidths) {
  static constexpr Dimensions kSquare20({
      .width = 20,
      .height = 20,
  });
  EXPECT_NE(kSmall, kSquare20);
  EXPECT_NE(kSquare20, kSmall);
}

TEST(DimensionsTest, EqualityForDifferentHeights) {
  static constexpr Dimensions kSquare10({
      .width = 20,
      .height = 20,
  });
  EXPECT_NE(kSmall, kSquare10);
  EXPECT_NE(kSquare10, kSmall);
}

TEST(DimensionsTest, FromDesignatedInitializer) {
  static constexpr Dimensions dimensions({
      .width = 400,
      .height = 500,
  });
  EXPECT_EQ(400, dimensions.width());
  EXPECT_EQ(500, dimensions.height());
}

TEST(DimensionsTest, FromUnsignedDesignatedInitializer) {
  static constexpr Dimensions dimensions({
      .unsigned_width = 400,
      .unsigned_height = 500,
  });
  EXPECT_EQ(400, dimensions.width());
  EXPECT_EQ(500, dimensions.height());
}

TEST(DimensionsTest, IsValidSmall) {
  EXPECT_TRUE(Dimensions::IsValid({.width = 10, .height = 20}));
  EXPECT_TRUE(Dimensions::IsValid({.unsigned_width = 10, .unsigned_height = 20}));
}

TEST(DimensionsTest, IsValidNegativeWidth) {
  EXPECT_FALSE(Dimensions::IsValid({.width = -10, .height = 20}));
  // The uint32_t overload doesn't need to handle negative inputs.
}

TEST(DimensionsTest, IsValidNegativeHeight) {
  EXPECT_FALSE(Dimensions::IsValid({.width = 10, .height = -20}));
  // The uint32_t overload doesn't need to handle negative inputs.
}

TEST(DimensionsTest, IsValidLargeWidth) {
  EXPECT_FALSE(Dimensions::IsValid({.width = 1'000'000, .height = 20}));
  EXPECT_FALSE(Dimensions::IsValid({.unsigned_width = 1'000'000, .unsigned_height = 20}));
}

TEST(DimensionsTest, IsValidLargeHeight) {
  EXPECT_FALSE(Dimensions::IsValid({.width = 10, .height = 1'000'000}));
  EXPECT_FALSE(Dimensions::IsValid({.unsigned_width = 10, .unsigned_height = 1'000'000}));
}

TEST(DimensionsTest, IsValidCanonicalEmpty) {
  EXPECT_TRUE(Dimensions::IsValid({.width = 0, .height = 0}));
  EXPECT_TRUE(Dimensions::IsValid({.unsigned_width = 0, .unsigned_height = 0}));
}

TEST(DimensionsTest, IsValidEmptyHorizontalLine) {
  EXPECT_FALSE(Dimensions::IsValid({.width = 10, .height = 0}));
  EXPECT_FALSE(Dimensions::IsValid({.unsigned_width = 10, .unsigned_height = 0}));
}

TEST(DimensionsTest, IsValidEmptyVerticalLine) {
  EXPECT_FALSE(Dimensions::IsValid({.width = 0, .height = 10}));
  EXPECT_FALSE(Dimensions::IsValid({.unsigned_width = 0, .unsigned_height = 10}));
}

TEST(DimensionsTest, IsEmpty) {
  static constexpr Dimensions kEmpty({.width = 0, .height = 0});
  EXPECT_TRUE(kEmpty.IsEmpty());

  EXPECT_FALSE(kSmall.IsEmpty());
  EXPECT_FALSE(kMedium.IsEmpty());
}

}  // namespace

}  // namespace display
