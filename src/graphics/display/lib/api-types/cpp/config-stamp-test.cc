// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(ConfigStamp, FidlConversion) {
  {
    EXPECT_EQ(ToConfigStamp(fuchsia_hardware_display::wire::ConfigStamp{.value = 1}),
              ConfigStamp(1));
    fuchsia_hardware_display::wire::ConfigStamp fidl_config_stamp =
        ToFidlConfigStamp(ConfigStamp(1));
    EXPECT_EQ(fidl_config_stamp.value, uint64_t{1});
  }

  {
    const uint64_t kLargeConfigStampValue = uint64_t{1} << 63;
    EXPECT_EQ(
        ToConfigStamp(fuchsia_hardware_display::wire::ConfigStamp{.value = kLargeConfigStampValue}),
        ConfigStamp(kLargeConfigStampValue));
    fuchsia_hardware_display::wire::ConfigStamp fidl_config_stamp =
        ToFidlConfigStamp(ConfigStamp(kLargeConfigStampValue));
    EXPECT_EQ(fidl_config_stamp.value, uint64_t{kLargeConfigStampValue});
  }

  {
    EXPECT_EQ(ToConfigStamp(fuchsia_hardware_display::wire::ConfigStamp{
                  .value = fuchsia_hardware_display::wire::kInvalidConfigStampValue}),
              kInvalidConfigStamp);
    fuchsia_hardware_display::wire::ConfigStamp fidl_config_stamp =
        ToFidlConfigStamp(kInvalidConfigStamp);
    EXPECT_EQ(fidl_config_stamp.value, fuchsia_hardware_display::wire::kInvalidConfigStampValue);
  }
}

}  // namespace

}  // namespace display
