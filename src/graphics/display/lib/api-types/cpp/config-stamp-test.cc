// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(ConfigStamp, Equality) {
  constexpr ConfigStamp kOne(1);
  constexpr ConfigStamp kAnotherOne(1);
  constexpr ConfigStamp kTwo(2);

  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_NE(kOne, kTwo);

  EXPECT_NE(kOne, kInvalidConfigStamp);
  EXPECT_NE(kTwo, kInvalidConfigStamp);
  EXPECT_EQ(kInvalidConfigStamp, kInvalidConfigStamp);
}

TEST(ConfigStamp, Compare) {
  constexpr ConfigStamp kOne(1);
  constexpr ConfigStamp kTwo(2);
  constexpr ConfigStamp kTwoToTheSixtyThird(uint64_t{1} << 63);

  EXPECT_LE(kOne, kOne);
  EXPECT_LE(kOne, kTwo);
  EXPECT_LE(kOne, kTwoToTheSixtyThird);
  EXPECT_LE(kTwo, kTwo);
  EXPECT_LE(kTwo, kTwoToTheSixtyThird);
  EXPECT_LE(kTwoToTheSixtyThird, kTwoToTheSixtyThird);

  EXPECT_LT(kOne, kTwo);
  EXPECT_LT(kOne, kTwoToTheSixtyThird);
  EXPECT_LT(kTwo, kTwoToTheSixtyThird);

  EXPECT_GE(kOne, kOne);
  EXPECT_GE(kTwo, kOne);
  EXPECT_GE(kTwoToTheSixtyThird, kOne);
  EXPECT_GE(kTwo, kTwo);
  EXPECT_GE(kTwoToTheSixtyThird, kTwo);
  EXPECT_GE(kTwoToTheSixtyThird, kTwoToTheSixtyThird);

  EXPECT_GT(kTwo, kOne);
  EXPECT_GT(kTwoToTheSixtyThird, kOne);
  EXPECT_GT(kTwoToTheSixtyThird, kTwo);
}

TEST(ConfigStamp, FidlConversion) {
  {
    EXPECT_EQ(ToConfigStamp(fuchsia_hardware_display_types::wire::ConfigStamp{.value = 1}),
              ConfigStamp(1));
    fuchsia_hardware_display_types::wire::ConfigStamp fidl_config_stamp =
        ToFidlConfigStamp(ConfigStamp(1));
    EXPECT_EQ(fidl_config_stamp.value, uint64_t{1});
  }

  {
    const uint64_t kLargeConfigStampValue = uint64_t{1} << 63;
    EXPECT_EQ(ToConfigStamp(fuchsia_hardware_display_types::wire::ConfigStamp{
                  .value = kLargeConfigStampValue}),
              ConfigStamp(kLargeConfigStampValue));
    fuchsia_hardware_display_types::wire::ConfigStamp fidl_config_stamp =
        ToFidlConfigStamp(ConfigStamp(kLargeConfigStampValue));
    EXPECT_EQ(fidl_config_stamp.value, uint64_t{kLargeConfigStampValue});
  }

  {
    EXPECT_EQ(ToConfigStamp(fuchsia_hardware_display_types::wire::ConfigStamp{
                  .value = INVALID_CONFIG_STAMP_VALUE}),
              kInvalidConfigStamp);
    fuchsia_hardware_display_types::wire::ConfigStamp fidl_config_stamp =
        ToFidlConfigStamp(kInvalidConfigStamp);
    EXPECT_EQ(fidl_config_stamp.value,
              fuchsia_hardware_display_types::wire::kInvalidConfigStampValue);
  }
}

}  // namespace

}  // namespace display
