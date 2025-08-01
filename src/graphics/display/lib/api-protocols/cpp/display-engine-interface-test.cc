// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"

#include <lib/stdcompat/span.h>

#include <type_traits>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"
#include "src/graphics/display/lib/api-protocols/cpp/mock-display-engine-for-out-of-tree.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"

namespace display {

namespace {

static_assert(!std::is_copy_constructible_v<DisplayEngineInterface>);
static_assert(!std::is_move_constructible_v<DisplayEngineInterface>);

static_assert(!std::is_final_v<DisplayEngineInterface>);
static_assert(std::is_polymorphic_v<DisplayEngineInterface>);

// Tests `DisplayEngineInterface` default implementation for out-of-tree
// drivers.
class DisplayEngineInterfaceOutOfTreeTest : public ::testing::Test {
 public:
  void TearDown() override { mock_display_engine_.CheckAllCallsReplayed(); }

 protected:
  testing::MockDisplayEngineForOutOfTree mock_display_engine_;
};

// Valid layer with properties aimed at testing API translation layers.
//
// The returned layer's properties are all different numbers. This is intended to
// help catch logic errors in API translation layers, such as swapping dimensions
// (width vs height) or size and position.
//
// `seed` is a small integer that results in small variations in the layer
// properties. This is intended to help catch logic errors in accessing and
// converting layers stored in arrays.
constexpr display::DriverLayer CreateValidLayerWithSeed(int seed) {
  const uint8_t color_blue = 0x40 + static_cast<uint8_t>(seed % 16);
  const uint8_t color_green = 0x50 + static_cast<uint8_t>((seed / 16) % 16);
  const uint8_t color_red = 0x60 + static_cast<uint8_t>(seed % 16);
  const uint8_t color_alpha = 0x70 + static_cast<uint8_t>((seed / 16) % 16);

  return display::DriverLayer({
      .display_destination = display::Rectangle(
          {.x = 100 + seed, .y = 200 + seed, .width = 300 + seed, .height = 400 + seed}),
      .image_source = display::Rectangle(
          {.x = 500 + seed, .y = 600 + seed, .width = 700 + seed, .height = 800 + seed}),
      .image_id = display::DriverImageId(8000 + seed),
      .image_metadata = display::ImageMetadata({.width = 2000 + seed,
                                                .height = 1000 + seed,
                                                .tiling_type = display::ImageTilingType::kLinear}),
      .fallback_color = display::Color(
          {.format = display::PixelFormat::kB8G8R8A8,
           .bytes = std::initializer_list<uint8_t>{color_blue, color_green, color_red, color_alpha,
                                                   0, 0, 0, 0}}),
      .image_source_transformation = display::CoordinateTransformation::kIdentity,
  });
}

TEST_F(DisplayEngineInterfaceOutOfTreeTest, CheckConfigurationIdentityColorConversionSuccess) {
  static constexpr display::DisplayId kDisplayId(1);
  static constexpr display::ModeId kModeId(2);
  static constexpr std::array<display::DriverLayer, 1> kLayers = {CreateValidLayerWithSeed(0)};

  mock_display_engine_.ExpectCheckConfiguration(
      [&](display::DisplayId display_id, display::ModeId mode_id,
          cpp20::span<const display::DriverLayer> layers) {
        EXPECT_EQ(kDisplayId, display_id);
        EXPECT_EQ(kModeId, mode_id);
        EXPECT_THAT(layers, ::testing::ElementsAreArray(kLayers));
        return display::ConfigCheckResult::kOk;
      });

  DisplayEngineInterface& display_engine = mock_display_engine_;
  EXPECT_EQ(display::ConfigCheckResult::kOk,
            display_engine.CheckConfiguration(kDisplayId, kModeId,
                                              display::ColorConversion::kIdentity, kLayers));
}

TEST_F(DisplayEngineInterfaceOutOfTreeTest, CheckConfigurationNonIdentityColorConversionError) {
  static constexpr display::DisplayId kDisplayId(1);
  static constexpr display::ModeId kModeId(2);
  const display::ColorConversion kColorConversion = {
      {
          .preoffsets = {1.0f, 2.0f, 3.0f},
          .coefficients =
              {
                  std::array<float, 3>{1.0f, 2.0f, 3.0f},
                  std::array<float, 3>{4.0f, 5.0f, 6.0f},
                  std::array<float, 3>{7.0f, 8.0f, 9.0f},
              },
          .postoffsets = {1.0f, 2.0f, 3.0f},
      },
  };
  static constexpr std::array<display::DriverLayer, 1> kLayers = {CreateValidLayerWithSeed(0)};
  //
  // The mock is not expected to be called because the default
  // `CheckConfiguration()` implementation in `DisplayEngineInterface` will
  // return `kUnsupportedConfig` when the color conversion is not identity.

  DisplayEngineInterface& display_engine = mock_display_engine_;
  EXPECT_EQ(display::ConfigCheckResult::kUnsupportedConfig,
            display_engine.CheckConfiguration(kDisplayId, kModeId, kColorConversion, kLayers));
}

TEST_F(DisplayEngineInterfaceOutOfTreeTest, ApplyConfigurationIdentityColorConversion) {
  static constexpr display::DisplayId kDisplayId(1);
  static constexpr display::ModeId kModeId(2);
  static constexpr std::array<display::DriverLayer, 1> kLayers = {CreateValidLayerWithSeed(0)};
  static constexpr display::DriverConfigStamp kConfigStamp(42);

  mock_display_engine_.ExpectApplyConfiguration(
      [&](display::DisplayId display_id, display::ModeId mode_id,
          cpp20::span<const display::DriverLayer> layers, display::DriverConfigStamp config_stamp) {
        EXPECT_EQ(kDisplayId, display_id);
        EXPECT_EQ(kModeId, mode_id);
        EXPECT_THAT(layers, ::testing::ElementsAreArray(kLayers));
        EXPECT_EQ(kConfigStamp, config_stamp);
      });

  DisplayEngineInterface& display_engine = mock_display_engine_;
  display_engine.ApplyConfiguration(kDisplayId, kModeId, display::ColorConversion::kIdentity,
                                    kLayers, kConfigStamp);
}

}  // namespace

}  // namespace display
