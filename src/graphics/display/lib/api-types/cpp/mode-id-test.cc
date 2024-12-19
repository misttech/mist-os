// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/mode-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr ModeId kOne(1);
constexpr ModeId kAnotherOne(1);
constexpr ModeId kTwo(2);

constexpr uint16_t kLargeIdValue = uint16_t{1} << 15;
constexpr ModeId kLargeId(kLargeIdValue);

TEST(ModeIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidModeId, kInvalidModeId);
}

TEST(ModeIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(ModeIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidModeId);
  EXPECT_NE(kTwo, kInvalidModeId);
  EXPECT_NE(kInvalidModeId, kOne);
  EXPECT_NE(kInvalidModeId, kTwo);
}

TEST(ModeIdTest, ToFidlModeId) {
  EXPECT_EQ(1u, ToFidlModeId(kOne).value);
  EXPECT_EQ(2u, ToFidlModeId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlModeId(kLargeId).value);
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kInvalidDispId,
            ToFidlModeId(kInvalidModeId).value);
}

TEST(ModeIdTest, ToBanjoModeId) {
  EXPECT_EQ(1u, ToBanjoModeId(kOne));
  EXPECT_EQ(2u, ToBanjoModeId(kTwo));
  EXPECT_EQ(kLargeIdValue, ToBanjoModeId(kLargeId));
  EXPECT_EQ(INVALID_DISPLAY_ID, ToBanjoModeId(kInvalidModeId));
}

TEST(ModeIdTest, ToModeIdWithFidlValue) {
  EXPECT_EQ(kOne, ToModeId(fuchsia_hardware_display_types::wire::ModeId{.value = 1}));
  EXPECT_EQ(kTwo, ToModeId(fuchsia_hardware_display_types::wire::ModeId{.value = 2}));
  EXPECT_EQ(kLargeId,
            ToModeId(fuchsia_hardware_display_types::wire::ModeId{.value = kLargeIdValue}));
  EXPECT_EQ(kInvalidModeId, ToModeId(fuchsia_hardware_display_types::wire::ModeId{
                                .value = fuchsia_hardware_display_types::wire::kInvalidDispId}));
}

TEST(ModeIdTest, ToModeIdWithBanjoValue) {
  EXPECT_EQ(kOne, ToModeId(1));
  EXPECT_EQ(kTwo, ToModeId(2));
  EXPECT_EQ(kLargeId, ToModeId(kLargeIdValue));
  EXPECT_EQ(kInvalidModeId, ToModeId(INVALID_MODE_ID));
}

TEST(ModeIdTest, FidlModeIdConversionRoundtrip) {
  EXPECT_EQ(kOne, ToModeId(ToFidlModeId(kOne)));
  EXPECT_EQ(kTwo, ToModeId(ToFidlModeId(kTwo)));
  EXPECT_EQ(kLargeId, ToModeId(ToFidlModeId(kLargeId)));
  EXPECT_EQ(kInvalidModeId, ToModeId(ToFidlModeId(kInvalidModeId)));
}

TEST(ModeIdTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, ToModeId(ToBanjoModeId(kOne)));
  EXPECT_EQ(kTwo, ToModeId(ToBanjoModeId(kTwo)));
  EXPECT_EQ(kLargeId, ToModeId(ToBanjoModeId(kLargeId)));
  EXPECT_EQ(kInvalidModeId, ToModeId(ToBanjoModeId(kInvalidModeId)));
}

}  // namespace

}  // namespace display
