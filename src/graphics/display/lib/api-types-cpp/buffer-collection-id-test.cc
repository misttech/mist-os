// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/buffer-collection-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

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

TEST(BufferCollectionIdTest, ToFidlBufferCollectionIdValue) {
  EXPECT_EQ(1u, ToFidlBufferCollectionIdValue(kOne));
  EXPECT_EQ(2u, ToFidlBufferCollectionIdValue(kTwo));
  EXPECT_EQ(kLargeIdValue, ToFidlBufferCollectionIdValue(kLargeId));
}

TEST(BufferCollectionIdTest, ToBufferCollectionIdWithFidlValue) {
  EXPECT_EQ(kOne, ToBufferCollectionId(1));
  EXPECT_EQ(kTwo, ToBufferCollectionId(2));
  EXPECT_EQ(kLargeId, ToBufferCollectionId(kLargeIdValue));
}

TEST(BufferCollectionIdTest, FidlBufferCollectionIdValueConversionRoundtrip) {
  EXPECT_EQ(kOne, ToBufferCollectionId(ToFidlBufferCollectionIdValue(kOne)));
  EXPECT_EQ(kTwo, ToBufferCollectionId(ToFidlBufferCollectionIdValue(kTwo)));
  EXPECT_EQ(kLargeId, ToBufferCollectionId(ToFidlBufferCollectionIdValue(kLargeId)));
}

}  // namespace

}  // namespace display
