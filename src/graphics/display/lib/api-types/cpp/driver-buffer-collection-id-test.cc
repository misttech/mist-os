// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr DriverBufferCollectionId kOne(1);
constexpr DriverBufferCollectionId kAnotherOne(1);
constexpr DriverBufferCollectionId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr DriverBufferCollectionId kLargeId(kLargeIdValue);

TEST(DriverBufferCollectionIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
}

TEST(DriverBufferCollectionIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(DriverBufferCollectionIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);
}

TEST(DriverBufferCollectionIdTest, ToBanjoDriverBufferCollectionId) {
  EXPECT_EQ(1u, ToBanjoDriverBufferCollectionId(kOne));
  EXPECT_EQ(2u, ToBanjoDriverBufferCollectionId(kTwo));
  EXPECT_EQ(kLargeIdValue, ToBanjoDriverBufferCollectionId(kLargeId));
}

TEST(DriverBufferCollectionIdTest, ToFidlDriverBufferCollectionId) {
  EXPECT_EQ(1u, ToFidlDriverBufferCollectionId(kOne).value);
  EXPECT_EQ(2u, ToFidlDriverBufferCollectionId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlDriverBufferCollectionId(kLargeId).value);
}

TEST(DriverBufferCollectionIdTest, ToDriverBufferCollectionIdWithBanjoValue) {
  EXPECT_EQ(kOne, ToDriverBufferCollectionId(1));
  EXPECT_EQ(kTwo, ToDriverBufferCollectionId(2));
  EXPECT_EQ(kLargeId, ToDriverBufferCollectionId(kLargeIdValue));
}

TEST(DriverBufferCollectionIdTest, ToDriverBufferCollectionIdWithFidlValue) {
  EXPECT_EQ(kOne, ToDriverBufferCollectionId(
                      fuchsia_hardware_display_engine::wire::BufferCollectionId{.value = 1}));
  EXPECT_EQ(kTwo, ToDriverBufferCollectionId(
                      fuchsia_hardware_display_engine::wire::BufferCollectionId{.value = 2}));
  EXPECT_EQ(kLargeId, ToDriverBufferCollectionId(kLargeIdValue));
}

TEST(DriverBufferCollectionIdTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDriverBufferCollectionId(ToBanjoDriverBufferCollectionId(kOne)));
  EXPECT_EQ(kTwo, ToDriverBufferCollectionId(ToBanjoDriverBufferCollectionId(kTwo)));
  EXPECT_EQ(kLargeId, ToDriverBufferCollectionId(ToBanjoDriverBufferCollectionId(kLargeId)));
}

TEST(DriverBufferCollectionIdTest, FidlConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDriverBufferCollectionId(ToFidlDriverBufferCollectionId(kOne)));
  EXPECT_EQ(kTwo, ToDriverBufferCollectionId(ToFidlDriverBufferCollectionId(kTwo)));
  EXPECT_EQ(kLargeId, ToDriverBufferCollectionId(ToFidlDriverBufferCollectionId(kLargeId)));
}

}  // namespace

}  // namespace display
