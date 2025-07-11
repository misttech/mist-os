// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr DriverCaptureImageId kOne(1);
constexpr DriverCaptureImageId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr DriverCaptureImageId kLargeId(kLargeIdValue);

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

TEST(DriverCaptureImageIdTest, ToFidlDriverCaptureImageId) {
  EXPECT_EQ(1u, ToFidlDriverCaptureImageId(kOne).value);
  EXPECT_EQ(2u, ToFidlDriverCaptureImageId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlDriverCaptureImageId(kLargeId).value);
  EXPECT_EQ(INVALID_ID, ToFidlDriverCaptureImageId(kInvalidDriverCaptureImageId).value);
}

}  // namespace

}  // namespace display
