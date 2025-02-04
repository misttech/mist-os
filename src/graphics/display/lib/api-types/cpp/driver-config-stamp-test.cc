// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr DriverConfigStamp kOne(1);
constexpr DriverConfigStamp kAnotherOne(1);
constexpr DriverConfigStamp kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr DriverConfigStamp kLargeId(kLargeIdValue);

TEST(DriverConfigStampTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
  EXPECT_EQ(kInvalidDriverConfigStamp, kInvalidDriverConfigStamp);
}

TEST(DriverConfigStampTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST(DriverConfigStampTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);

  EXPECT_NE(kOne, kInvalidDriverConfigStamp);
  EXPECT_NE(kTwo, kInvalidDriverConfigStamp);
  EXPECT_NE(kInvalidDriverConfigStamp, kOne);
  EXPECT_NE(kInvalidDriverConfigStamp, kTwo);
}

TEST(DriverConfigStampTest, ToBanjoDriverConfigStamp) {
  EXPECT_EQ(1u, ToBanjoDriverConfigStamp(kOne).value);
  EXPECT_EQ(2u, ToBanjoDriverConfigStamp(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToBanjoDriverConfigStamp(kLargeId).value);
  EXPECT_EQ(INVALID_CONFIG_STAMP_VALUE, ToBanjoDriverConfigStamp(kInvalidDriverConfigStamp).value);
}

TEST(DriverConfigStampTest, ToFidlDriverConfigStamp) {
  EXPECT_EQ(1u, ToFidlDriverConfigStamp(kOne).value);
  EXPECT_EQ(2u, ToFidlDriverConfigStamp(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlDriverConfigStamp(kLargeId).value);
  EXPECT_EQ(INVALID_CONFIG_STAMP_VALUE, ToFidlDriverConfigStamp(kInvalidDriverConfigStamp).value);
}

TEST(DriverConfigStampTest, ToDriverConfigStampWithBanjoValue) {
  EXPECT_EQ(kOne, ToDriverConfigStamp(config_stamp_t{.value = 1}));
  EXPECT_EQ(kTwo, ToDriverConfigStamp(config_stamp_t{.value = 2}));
  EXPECT_EQ(kLargeId, ToDriverConfigStamp(config_stamp_t{.value = kLargeIdValue}));
  EXPECT_EQ(kInvalidDriverConfigStamp,
            ToDriverConfigStamp(config_stamp_t{.value = INVALID_CONFIG_STAMP_VALUE}));
}

TEST(DriverConfigStampTest, ToDriverConfigStampWithFidlValue) {
  EXPECT_EQ(kOne,
            ToDriverConfigStamp(fuchsia_hardware_display_engine::wire::ConfigStamp{.value = 1}));
  EXPECT_EQ(kTwo,
            ToDriverConfigStamp(fuchsia_hardware_display_engine::wire::ConfigStamp{.value = 2}));
  EXPECT_EQ(kLargeId, ToDriverConfigStamp(fuchsia_hardware_display_engine::wire::ConfigStamp{
                          .value = kLargeIdValue}));
  EXPECT_EQ(kInvalidDriverConfigStamp,
            ToDriverConfigStamp(fuchsia_hardware_display_engine::wire::ConfigStamp{
                .value = fuchsia_hardware_display_engine::wire::kInvalidConfigStampValue}));
}

TEST(DriverConfigStampTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDriverConfigStamp(ToBanjoDriverConfigStamp(kOne)));
  EXPECT_EQ(kTwo, ToDriverConfigStamp(ToBanjoDriverConfigStamp(kTwo)));
  EXPECT_EQ(kLargeId, ToDriverConfigStamp(ToBanjoDriverConfigStamp(kLargeId)));
  EXPECT_EQ(kInvalidDriverConfigStamp,
            ToDriverConfigStamp(ToBanjoDriverConfigStamp(kInvalidDriverConfigStamp)));
}

TEST(DriverConfigStampTest, FidlConversionRoundtrip) {
  EXPECT_EQ(kOne, ToDriverConfigStamp(ToFidlDriverConfigStamp(kOne)));
  EXPECT_EQ(kTwo, ToDriverConfigStamp(ToFidlDriverConfigStamp(kTwo)));
  EXPECT_EQ(kLargeId, ToDriverConfigStamp(ToFidlDriverConfigStamp(kLargeId)));
  EXPECT_EQ(kInvalidDriverConfigStamp,
            ToDriverConfigStamp(ToFidlDriverConfigStamp(kInvalidDriverConfigStamp)));
}

}  // namespace

}  // namespace display
