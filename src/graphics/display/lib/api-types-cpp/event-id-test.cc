// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/event-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr EventId kOne(1);
constexpr EventId kAnotherOne(1);
constexpr EventId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr EventId kLargeId(kLargeIdValue);

TEST(EventIdTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidEventId, kInvalidEventId);
}

TEST(EventIdTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(EventIdTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidEventId);
  EXPECT_NE(kTwo, kInvalidEventId);
  EXPECT_NE(kInvalidEventId, kOne);
  EXPECT_NE(kInvalidEventId, kTwo);
}

TEST(EventIdTest, ToFidlEventId) {
  EXPECT_EQ(1u, ToFidlEventId(kOne).value);
  EXPECT_EQ(2u, ToFidlEventId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlEventId(kLargeId).value);
  EXPECT_EQ(fuchsia_hardware_display::wire::kInvalidDispId, ToFidlEventId(kInvalidEventId).value);
}

TEST(EventIdTest, ToFidlEventIdValue) {
  EXPECT_EQ(1u, ToFidlEventIdValue(kOne));
  EXPECT_EQ(2u, ToFidlEventIdValue(kTwo));
  EXPECT_EQ(kLargeIdValue, ToFidlEventIdValue(kLargeId));
  EXPECT_EQ(fuchsia_hardware_display::wire::kInvalidDispId, ToFidlEventIdValue(kInvalidEventId));
}

TEST(EventIdTest, ToEventIdWithFidlEventId) {
  EXPECT_EQ(kOne, ToEventId(fuchsia_hardware_display::wire::EventId{.value = 1}));
  EXPECT_EQ(kTwo, ToEventId(fuchsia_hardware_display::wire::EventId{.value = 2}));
  EXPECT_EQ(kLargeId, ToEventId(fuchsia_hardware_display::wire::EventId{.value = kLargeIdValue}));
  EXPECT_EQ(kInvalidEventId, ToEventId(fuchsia_hardware_display::wire::EventId{
                                 .value = fuchsia_hardware_display::wire::kInvalidDispId}));
}

TEST(EventIdTest, ToEventIdWithFidlValue) {
  EXPECT_EQ(kOne, ToEventId(1));
  EXPECT_EQ(kTwo, ToEventId(2));
  EXPECT_EQ(kLargeId, ToEventId(kLargeIdValue));
  EXPECT_EQ(kInvalidEventId, ToEventId(fuchsia_hardware_display::wire::kInvalidDispId));
}

TEST(EventIdTest, FidlEventIdConversionRoundtrip) {
  EXPECT_EQ(kOne, ToEventId(ToFidlEventId(kOne)));
  EXPECT_EQ(kTwo, ToEventId(ToFidlEventId(kTwo)));
  EXPECT_EQ(kLargeId, ToEventId(ToFidlEventId(kLargeId)));
  EXPECT_EQ(kInvalidEventId, ToEventId(ToFidlEventId(kInvalidEventId)));
}

TEST(EventIdTest, FidlEventIdValueConversionRoundtrip) {
  EXPECT_EQ(kOne, ToEventId(ToFidlEventIdValue(kOne)));
  EXPECT_EQ(kTwo, ToEventId(ToFidlEventIdValue(kTwo)));
  EXPECT_EQ(kLargeId, ToEventId(ToFidlEventIdValue(kLargeId)));
  EXPECT_EQ(kInvalidEventId, ToEventId(ToFidlEventIdValue(kInvalidEventId)));
}

}  // namespace

}  // namespace display
