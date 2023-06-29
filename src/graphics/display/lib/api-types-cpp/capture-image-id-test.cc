// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/capture-image-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr CaptureImageId kOne(1);
constexpr CaptureImageId kAnotherOne(1);
constexpr CaptureImageId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr CaptureImageId kLargeId(kLargeIdValue);

TEST(CaptureImageIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidCaptureImageId, kInvalidCaptureImageId);
}

TEST(CaptureImageIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(CaptureImageIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidCaptureImageId);
  EXPECT_NE(kTwo, kInvalidCaptureImageId);
  EXPECT_NE(kInvalidCaptureImageId, kOne);
  EXPECT_NE(kInvalidCaptureImageId, kTwo);
}

TEST(CaptureImageIdTest, ToFidlCaptureImageIdValue) {
  EXPECT_EQ(1u, ToFidlCaptureImageIdValue(kOne));
  EXPECT_EQ(2u, ToFidlCaptureImageIdValue(kTwo));
  EXPECT_EQ(kLargeIdValue, ToFidlCaptureImageIdValue(kLargeId));
  EXPECT_EQ(fuchsia_hardware_display::wire::kInvalidDispId,
            ToFidlCaptureImageIdValue(kInvalidCaptureImageId));
}

TEST(CaptureImageIdTest, ToCaptureImageIdWithFidlValue) {
  EXPECT_EQ(kOne, ToCaptureImageId(1));
  EXPECT_EQ(kTwo, ToCaptureImageId(2));
  EXPECT_EQ(kLargeId, ToCaptureImageId(kLargeIdValue));
  EXPECT_EQ(kInvalidCaptureImageId,
            ToCaptureImageId(fuchsia_hardware_display::wire::kInvalidDispId));
}

TEST(CaptureImageIdTest, FidlCaptureImageIdValueConversionRoundtrip) {
  EXPECT_EQ(kOne, ToCaptureImageId(ToFidlCaptureImageIdValue(kOne)));
  EXPECT_EQ(kTwo, ToCaptureImageId(ToFidlCaptureImageIdValue(kTwo)));
  EXPECT_EQ(kLargeId, ToCaptureImageId(ToFidlCaptureImageIdValue(kLargeId)));
  EXPECT_EQ(kInvalidCaptureImageId,
            ToCaptureImageId(ToFidlCaptureImageIdValue(kInvalidCaptureImageId)));
}

}  // namespace

}  // namespace display
