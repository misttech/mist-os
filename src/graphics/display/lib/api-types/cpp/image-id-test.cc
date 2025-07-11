// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/image-id.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr ImageId kOne(1);
constexpr ImageId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr ImageId kLargeId(kLargeIdValue);

TEST(ImageIdTest, ToFidlImageId) {
  EXPECT_EQ(1u, ToFidlImageId(kOne).value);
  EXPECT_EQ(2u, ToFidlImageId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlImageId(kLargeId).value);
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kInvalidDispId,
            ToFidlImageId(kInvalidImageId).value);
}

TEST(ImageIdTest, ToImageIdWithFidlValue) {
  EXPECT_EQ(kOne, ToImageId(fuchsia_hardware_display::wire::ImageId{.value = 1}));
  EXPECT_EQ(kTwo, ToImageId(fuchsia_hardware_display::wire::ImageId{.value = 2}));
  EXPECT_EQ(kLargeId, ToImageId(fuchsia_hardware_display::wire::ImageId{.value = kLargeIdValue}));
  EXPECT_EQ(kInvalidImageId, ToImageId(fuchsia_hardware_display::wire::ImageId{
                                 .value = fuchsia_hardware_display_types::wire::kInvalidDispId}));
}

TEST(ImageIdTest, FidlImageIdConversionRoundtrip) {
  EXPECT_EQ(kOne, ToImageId(ToFidlImageId(kOne)));
  EXPECT_EQ(kTwo, ToImageId(ToFidlImageId(kTwo)));
  EXPECT_EQ(kLargeId, ToImageId(ToFidlImageId(kLargeId)));
  EXPECT_EQ(kInvalidImageId, ToImageId(ToFidlImageId(kInvalidImageId)));
}

}  // namespace

}  // namespace display
