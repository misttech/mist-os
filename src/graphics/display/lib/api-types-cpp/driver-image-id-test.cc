// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr DriverImageId kOne(1);
constexpr DriverImageId kAnotherOne(1);
constexpr DriverImageId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr DriverImageId kLargeId(kLargeIdValue);

TEST(DriverImageIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidDriverImageId, kInvalidDriverImageId);
}

TEST(DriverImageIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(DriverImageIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidDriverImageId);
  EXPECT_NE(kTwo, kInvalidDriverImageId);
  EXPECT_NE(kInvalidDriverImageId, kOne);
  EXPECT_NE(kInvalidDriverImageId, kTwo);
}

TEST(DriverImageIdTest, ToBanjoDriverImageId) {
  EXPECT_EQ(1u, ToBanjoDriverImageId(kOne));
  EXPECT_EQ(2u, ToBanjoDriverImageId(kTwo));
  EXPECT_EQ(kLargeIdValue, ToBanjoDriverImageId(kLargeId));
  EXPECT_EQ(INVALID_ID, ToBanjoDriverImageId(kInvalidDriverImageId));
}

TEST(DriverImageIdTest, ToDriverImageIdWithBanjoValue) {
  EXPECT_EQ(kOne, ToDriverImageId(1));
  EXPECT_EQ(kTwo, ToDriverImageId(2));
  EXPECT_EQ(kLargeId, ToDriverImageId(kLargeIdValue));
  EXPECT_EQ(kInvalidDriverImageId, ToDriverImageId(INVALID_ID));
}

TEST(DriverImageIdTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDriverImageId(ToBanjoDriverImageId(kOne)));
  EXPECT_EQ(kTwo, ToDriverImageId(ToBanjoDriverImageId(kTwo)));
  EXPECT_EQ(kLargeId, ToDriverImageId(ToBanjoDriverImageId(kLargeId)));
  EXPECT_EQ(kInvalidDriverImageId, ToDriverImageId(ToBanjoDriverImageId(kInvalidDriverImageId)));
}

}  // namespace

}  // namespace display
