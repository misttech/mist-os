// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/layer-id.h"

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr LayerId kOne(1);
constexpr LayerId kAnotherOne(1);
constexpr LayerId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr LayerId kLargeId(kLargeIdValue);

TEST(LayerIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidLayerId, kInvalidLayerId);
}

TEST(LayerIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(LayerIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidLayerId);
  EXPECT_NE(kTwo, kInvalidLayerId);
  EXPECT_NE(kInvalidLayerId, kOne);
  EXPECT_NE(kInvalidLayerId, kTwo);
}

TEST(LayerIdTest, ToFidlLayerId) {
  EXPECT_EQ(1u, ToFidlLayerId(kOne));
  EXPECT_EQ(2u, ToFidlLayerId(kTwo));
  EXPECT_EQ(kLargeIdValue, ToFidlLayerId(kLargeId));
  EXPECT_EQ(fuchsia_hardware_display::wire::kInvalidDispId, ToFidlLayerId(kInvalidLayerId));
}

TEST(LayerIdTest, ToLayerIdWithFidlValue) {
  EXPECT_EQ(kOne, ToLayerId(1));
  EXPECT_EQ(kTwo, ToLayerId(2));
  EXPECT_EQ(kLargeId, ToLayerId(kLargeIdValue));
  EXPECT_EQ(kInvalidLayerId, ToLayerId(fuchsia_hardware_display::wire::kInvalidDispId));
}

TEST(LayerIdTest, FidlConversionRoundtrip) {
  EXPECT_EQ(kOne, ToLayerId(ToFidlLayerId(kOne)));
  EXPECT_EQ(kTwo, ToLayerId(ToFidlLayerId(kTwo)));
  EXPECT_EQ(kLargeId, ToLayerId(ToFidlLayerId(kLargeId)));
  EXPECT_EQ(kInvalidLayerId, ToLayerId(ToFidlLayerId(kInvalidLayerId)));
}

}  // namespace

}  // namespace display
