// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/buffer-collection-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr BufferCollectionId kOne(1);
constexpr BufferCollectionId kAnotherOne(1);
constexpr BufferCollectionId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr BufferCollectionId kLargeId(kLargeIdValue);

TEST(BufferCollectionIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
}

TEST(BufferCollectionIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(BufferCollectionIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);
}

TEST(BufferCollectionIdTest, ToFidlBufferCollectionId) {
  EXPECT_EQ(1u, ToFidlBufferCollectionId(kOne).value);
  EXPECT_EQ(2u, ToFidlBufferCollectionId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlBufferCollectionId(kLargeId).value);
}

TEST(BufferCollectionIdTest, ToBufferCollectionIdWithFidlValue) {
  EXPECT_EQ(kOne,
            ToBufferCollectionId(fuchsia_hardware_display::wire::BufferCollectionId{.value = 1}));
  EXPECT_EQ(kTwo,
            ToBufferCollectionId(fuchsia_hardware_display::wire::BufferCollectionId{.value = 2}));
  EXPECT_EQ(kLargeId, ToBufferCollectionId(fuchsia_hardware_display::wire::BufferCollectionId{
                          .value = kLargeIdValue}));
}

TEST(BufferCollectionIdTest, FidlBufferCollectionIdConversionRoundtrip) {
  EXPECT_EQ(kOne, ToBufferCollectionId(ToFidlBufferCollectionId(kOne)));
  EXPECT_EQ(kTwo, ToBufferCollectionId(ToFidlBufferCollectionId(kTwo)));
  EXPECT_EQ(kLargeId, ToBufferCollectionId(ToFidlBufferCollectionId(kLargeId)));
}

}  // namespace

}  // namespace display
