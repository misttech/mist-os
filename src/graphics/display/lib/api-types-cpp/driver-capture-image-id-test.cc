// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr DriverCaptureImageId kOne(1);
constexpr DriverCaptureImageId kAnotherOne(1);
constexpr DriverCaptureImageId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr DriverCaptureImageId kLargeId(kLargeIdValue);

TEST(DriverCaptureImageIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidDriverCaptureImageId, kInvalidDriverCaptureImageId);
}

TEST(DriverCaptureImageIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(DriverCaptureImageIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidDriverCaptureImageId);
  EXPECT_NE(kTwo, kInvalidDriverCaptureImageId);
  EXPECT_NE(kInvalidDriverCaptureImageId, kOne);
  EXPECT_NE(kInvalidDriverCaptureImageId, kTwo);
}

TEST(DriverCaptureImageIdTest, ToBanjoDriverCaptureImageId) {
  EXPECT_EQ(1u, ToBanjoDriverCaptureImageId(kOne));
  EXPECT_EQ(2u, ToBanjoDriverCaptureImageId(kTwo));
  EXPECT_EQ(kLargeIdValue, ToBanjoDriverCaptureImageId(kLargeId));
  EXPECT_EQ(INVALID_ID, ToBanjoDriverCaptureImageId(kInvalidDriverCaptureImageId));
}

TEST(DriverCaptureImageIdTest, ToDriverCaptureImageIdWithBanjoValue) {
  EXPECT_EQ(kOne, ToDriverCaptureImageId(1));
  EXPECT_EQ(kTwo, ToDriverCaptureImageId(2));
  EXPECT_EQ(kLargeId, ToDriverCaptureImageId(kLargeIdValue));
  EXPECT_EQ(kInvalidDriverCaptureImageId, ToDriverCaptureImageId(INVALID_ID));
}

TEST(DriverCaptureImageIdTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDriverCaptureImageId(ToBanjoDriverCaptureImageId(kOne)));
  EXPECT_EQ(kTwo, ToDriverCaptureImageId(ToBanjoDriverCaptureImageId(kTwo)));
  EXPECT_EQ(kLargeId, ToDriverCaptureImageId(ToBanjoDriverCaptureImageId(kLargeId)));
  EXPECT_EQ(kInvalidDriverCaptureImageId,
            ToDriverCaptureImageId(ToBanjoDriverCaptureImageId(kInvalidDriverCaptureImageId)));
}

}  // namespace

}  // namespace display
