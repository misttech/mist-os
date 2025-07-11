// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/layer-id.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr LayerId kOne(1);
constexpr LayerId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr LayerId kLargeId(kLargeIdValue);

TEST(LayerIdTest, ToFidlLayerId) {
  EXPECT_EQ(1u, ToFidlLayerId(kOne).value);
  EXPECT_EQ(2u, ToFidlLayerId(kTwo).value);
  EXPECT_EQ(kLargeIdValue, ToFidlLayerId(kLargeId).value);
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kInvalidDispId,
            ToFidlLayerId(kInvalidLayerId).value);
}
TEST(LayerIdTest, ToLayerIdWithFidlValue) {
  EXPECT_EQ(kOne, ToLayerId(fuchsia_hardware_display::wire::LayerId{1}));
  EXPECT_EQ(kTwo, ToLayerId(fuchsia_hardware_display::wire::LayerId{2}));
  EXPECT_EQ(kLargeId, ToLayerId(fuchsia_hardware_display::wire::LayerId{kLargeIdValue}));
  EXPECT_EQ(kInvalidLayerId, ToLayerId(fuchsia_hardware_display::wire::LayerId{
                                 fuchsia_hardware_display_types::wire::kInvalidDispId}));
}

TEST(LayerIdTest, FidlLayerIdConversionRoundtrip) {
  EXPECT_EQ(kOne, ToLayerId(ToFidlLayerId(kOne)));
  EXPECT_EQ(kTwo, ToLayerId(ToFidlLayerId(kTwo)));
  EXPECT_EQ(kLargeId, ToLayerId(ToFidlLayerId(kLargeId)));
  EXPECT_EQ(kInvalidLayerId, ToLayerId(ToFidlLayerId(kInvalidLayerId)));
}

}  // namespace

}  // namespace display
