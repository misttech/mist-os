// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/display-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr DisplayId kOne(1);
constexpr DisplayId kAnotherOne(1);
constexpr DisplayId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr DisplayId kLargeId(kLargeIdValue);

TEST(DisplayIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidDisplayId, kInvalidDisplayId);
}

TEST(DisplayIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(DisplayIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidDisplayId);
  EXPECT_NE(kTwo, kInvalidDisplayId);
  EXPECT_NE(kInvalidDisplayId, kOne);
  EXPECT_NE(kInvalidDisplayId, kTwo);
}

TEST(DisplayIdTest, ToFidlDisplayId) {
  EXPECT_EQ(1u, ToFidlDisplayId(kOne).value);
  EXPECT_EQ(2u, ToFidlDisplayId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlDisplayId(kLargeId).value);
  EXPECT_EQ(fuchsia_hardware_display::wire::kInvalidDispId,
            ToFidlDisplayId(kInvalidDisplayId).value);
}

TEST(DisplayIdTest, ToBanjoDisplayId) {
  EXPECT_EQ(1u, ToBanjoDisplayId(kOne));
  EXPECT_EQ(2u, ToBanjoDisplayId(kTwo));
  EXPECT_EQ(kLargeIdValue, ToBanjoDisplayId(kLargeId));
  EXPECT_EQ(INVALID_DISPLAY_ID, ToBanjoDisplayId(kInvalidDisplayId));
}

TEST(DisplayIdTest, ToDisplayIdWithFidlValue) {
  EXPECT_EQ(kOne, ToDisplayId(fuchsia_hardware_display::wire::DisplayId{.value = 1}));
  EXPECT_EQ(kTwo, ToDisplayId(fuchsia_hardware_display::wire::DisplayId{.value = 2}));
  EXPECT_EQ(kLargeId,
            ToDisplayId(fuchsia_hardware_display::wire::DisplayId{.value = kLargeIdValue}));
  EXPECT_EQ(kInvalidDisplayId, ToDisplayId(fuchsia_hardware_display::wire::DisplayId{
                                   .value = fuchsia_hardware_display::wire::kInvalidDispId}));
}

TEST(DisplayIdTest, ToDisplayIdWithBanjoValue) {
  EXPECT_EQ(kOne, ToDisplayId(1));
  EXPECT_EQ(kTwo, ToDisplayId(2));
  EXPECT_EQ(kLargeId, ToDisplayId(kLargeIdValue));
  EXPECT_EQ(kInvalidDisplayId, ToDisplayId(INVALID_DISPLAY_ID));
}

TEST(DisplayIdTest, FidlDisplayIdConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDisplayId(ToFidlDisplayId(kOne)));
  EXPECT_EQ(kTwo, ToDisplayId(ToFidlDisplayId(kTwo)));
  EXPECT_EQ(kLargeId, ToDisplayId(ToFidlDisplayId(kLargeId)));
  EXPECT_EQ(kInvalidDisplayId, ToDisplayId(ToFidlDisplayId(kInvalidDisplayId)));
}

TEST(DisplayIdTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDisplayId(ToBanjoDisplayId(kOne)));
  EXPECT_EQ(kTwo, ToDisplayId(ToBanjoDisplayId(kTwo)));
  EXPECT_EQ(kLargeId, ToDisplayId(ToBanjoDisplayId(kLargeId)));
  EXPECT_EQ(kInvalidDisplayId, ToDisplayId(ToBanjoDisplayId(kInvalidDisplayId)));
}

}  // namespace

}  // namespace display
