// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/banjo-fidl-conversion.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/fdf/cpp/arena.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"

namespace display_coordinator {

namespace {

TEST(BanjoFidlConversionTest, ToFidlDisplayConfig) {
  static constexpr layer_t kBanjoLayer0 = {
      .display_destination = {.x = 10, .y = 20, .width = 300, .height = 400},
      .image_source = {.x = 30, .y = 40, .width = 500, .height = 600},
      .image_handle = 4242,
      .image_metadata = {.dimensions = {.width = 700, .height = 800},
                         .tiling_type = IMAGE_TILING_TYPE_LINEAR},
      .fallback_color = {.format = static_cast<fuchsia_images2_pixel_format_enum_value_t>(
                             fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
                         .bytes = {0xff, 0xdd, 0xcc, 0xbb, 0, 0, 0, 0}},
      .alpha_mode = ALPHA_PREMULTIPLIED,
      .alpha_layer_val = 0.25f,
      .image_source_transformation = COORDINATE_TRANSFORMATION_REFLECT_X,
  };
  static constexpr layer_t kBanjoLayer1 = {
      .display_destination = {.x = 11, .y = 22, .width = 303, .height = 404},
      .image_source = {.x = 33, .y = 44, .width = 505, .height = 606},
      .image_handle = 2424,
      .image_metadata = {.dimensions = {.width = 707, .height = 808},
                         .tiling_type = IMAGE_TILING_TYPE_LINEAR},
      .fallback_color = {.format = static_cast<fuchsia_images2_pixel_format_enum_value_t>(
                             fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
                         .bytes = {0xaa, 0x99, 0x88, 0x77, 0, 0, 0, 0}},
      .alpha_mode = ALPHA_HW_MULTIPLY,
      .alpha_layer_val = 0.75f,
      .image_source_transformation = COORDINATE_TRANSFORMATION_REFLECT_Y,
  };
  constexpr layer_t kBanjoLayers[] = {kBanjoLayer0, kBanjoLayer1};

  const display_config_t kBanjoConfig = {
      .display_id = 1,
      .mode_id = 2,
      .timing =
          {
              .h_addressable = 1920,
              .v_addressable = 1080,
          },
      .color_conversion =
          {
              .preoffsets = {0.1f, 0.2f, 0.3f},
              .coefficients =
                  {
                      {1.0f, 2.0f, 3.0f},
                      {4.0f, 5.0f, 6.0f},
                      {7.0f, 8.0f, 9.0f},
                  },
              .postoffsets = {0.4f, 0.5f, 0.6f},
          },
      .layers_list = kBanjoLayers,
      .layers_count = 2,
  };

  fdf::Arena arena('TEST');
  fuchsia_hardware_display_engine::wire::DisplayConfig fidl_config =
      ToFidlDisplayConfig(kBanjoConfig, arena);

  EXPECT_EQ(fidl_config.display_id.value, 1u);
  EXPECT_EQ(fidl_config.mode_id.value, 2u);
  EXPECT_EQ(fidl_config.timing.h_addressable, 1920u);
  EXPECT_EQ(fidl_config.timing.v_addressable, 1080u);
  EXPECT_THAT(fidl_config.color_conversion.preoffsets, ::testing::ElementsAre(0.1f, 0.2f, 0.3f));
  ASSERT_EQ(fidl_config.color_conversion.coefficients.size(), 3u);
  EXPECT_THAT(fidl_config.color_conversion.coefficients[0],
              ::testing::ElementsAre(1.0f, 2.0f, 3.0f));
  EXPECT_THAT(fidl_config.color_conversion.coefficients[1],
              ::testing::ElementsAre(4.0f, 5.0f, 6.0f));
  EXPECT_THAT(fidl_config.color_conversion.coefficients[2],
              ::testing::ElementsAre(7.0f, 8.0f, 9.0f));
  EXPECT_THAT(fidl_config.color_conversion.postoffsets, ::testing::ElementsAre(0.4f, 0.5f, 0.6f));

  ASSERT_EQ(fidl_config.layers.size(), 2u);
  EXPECT_EQ(display::DriverLayer(fidl_config.layers[0]), display::DriverLayer(kBanjoLayer0));
  EXPECT_EQ(display::DriverLayer(fidl_config.layers[1]), display::DriverLayer(kBanjoLayer1));
}

}  // namespace

}  // namespace display_coordinator
