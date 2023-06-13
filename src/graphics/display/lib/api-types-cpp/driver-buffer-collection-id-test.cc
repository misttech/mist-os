// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(DriverBufferCollectionId, Equality) {
  constexpr DriverBufferCollectionId kOne(1);
  constexpr DriverBufferCollectionId kAnotherOne(1);
  constexpr DriverBufferCollectionId kTwo(2);

  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_NE(kOne, kTwo);
}

TEST(DriverBufferCollectionId, BanjoConversion) {
  EXPECT_EQ(ToDriverBufferCollectionId(1), DriverBufferCollectionId(1));
  EXPECT_EQ(ToDriverBufferCollectionId(1).value(), uint64_t{1});
  EXPECT_EQ(ToBanjoDriverBufferCollectionId(DriverBufferCollectionId(1)), uint64_t{1});

  const uint64_t kLargeBufferCollectionIdValue = uint64_t{1} << 63;
  EXPECT_EQ(ToDriverBufferCollectionId(kLargeBufferCollectionIdValue).value(),
            kLargeBufferCollectionIdValue);
  EXPECT_EQ(
      ToBanjoDriverBufferCollectionId(DriverBufferCollectionId(kLargeBufferCollectionIdValue)),
      kLargeBufferCollectionIdValue);
}

}  // namespace

}  // namespace display
