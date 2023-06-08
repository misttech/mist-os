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

TEST(DisplayId, Equality) {
  constexpr DisplayId kOne(1);
  constexpr DisplayId kAnotherOne(1);
  constexpr DisplayId kTwo(2);

  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_NE(kOne, kTwo);

  EXPECT_NE(kOne, kInvalidDisplayId);
  EXPECT_NE(kTwo, kInvalidDisplayId);
  EXPECT_EQ(kInvalidDisplayId, kInvalidDisplayId);
}

TEST(DisplayId, BanjoConversion) {
  EXPECT_EQ(ToDisplayId(1), DisplayId(1));
  EXPECT_EQ(ToDisplayId(1).value(), uint64_t{1});
  EXPECT_EQ(ToBanjoDisplayId(DisplayId(1)), uint64_t{1});

  const uint64_t kLargeDisplayIdValue = uint64_t{1} << 63;
  EXPECT_EQ(ToDisplayId(kLargeDisplayIdValue).value(), kLargeDisplayIdValue);
  EXPECT_EQ(ToBanjoDisplayId(DisplayId(kLargeDisplayIdValue)), kLargeDisplayIdValue);

  EXPECT_EQ(ToDisplayId(INVALID_DISPLAY_ID), kInvalidDisplayId);
  EXPECT_EQ(ToDisplayId(INVALID_DISPLAY_ID).value(), INVALID_DISPLAY_ID);
  EXPECT_EQ(ToBanjoDisplayId(kInvalidDisplayId), INVALID_DISPLAY_ID);
}

TEST(DisplayId, FidlConversion) {
  EXPECT_EQ(ToFidlDisplayId(DisplayId(1)), uint64_t{1});

  const uint64_t kLargeDisplayIdValue = uint64_t{1} << 63;
  EXPECT_EQ(ToFidlDisplayId(DisplayId(kLargeDisplayIdValue)), kLargeDisplayIdValue);

  EXPECT_EQ(ToFidlDisplayId(kInvalidDisplayId), fuchsia_hardware_display::wire::kInvalidDispId);
}

}  // namespace

}  // namespace display
