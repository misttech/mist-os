// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-banjo-adapter.h"

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <initializer_list>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-protocols/cpp/mock-display-engine.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/coordinate-transformation.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"
#include "src/graphics/display/lib/api-types/cpp/layer-composition-operations.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"
#include "src/graphics/display/lib/api-types/cpp/rectangle.h"
#include "src/lib/testing/predicates/status.h"

namespace display {

namespace {

class DisplayEngineBanjoAdapterTest : public ::testing::Test {
 public:
  void TearDown() override { mock_.CheckAllAccessesReplayed(); }

 protected:
  display::testing::MockDisplayEngine mock_;
  display::DisplayEngineEventsBanjo engine_events_banjo_;
  display::DisplayEngineBanjoAdapter banjo_adapter_{&mock_, &engine_events_banjo_};
  const display_engine_protocol_t display_engine_protocol_ = banjo_adapter_.GetProtocol();
  ddk::DisplayEngineProtocolClient engine_banjo_{&display_engine_protocol_};
};

TEST_F(DisplayEngineBanjoAdapterTest, ImportBufferCollectionSuccess) {
  auto [buffer_collection_token_client, buffer_collection_token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  const zx_handle_t buffer_collection_token_client_handle =
      buffer_collection_token_client.handle()->get();

  mock_.ExpectImportBufferCollection(
      [&](display::DriverBufferCollectionId buffer_collection_id,
          fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) {
        EXPECT_EQ(kBufferCollectionId, buffer_collection_id);
        EXPECT_EQ(buffer_collection_token_client_handle, buffer_collection_token.handle()->get());
        return zx::ok();
      });
  EXPECT_OK(engine_banjo_.ImportBufferCollection(
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId),
      std::move(buffer_collection_token_client).TakeChannel()));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportBufferCollectionError) {
  auto [buffer_collection_token_client, buffer_collection_token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  mock_.ExpectImportBufferCollection(
      [&](display::DriverBufferCollectionId buffer_collection_id,
          fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) {
        return zx::error(ZX_ERR_INTERNAL);
      });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.ImportBufferCollection(
                                 display::ToBanjoDriverBufferCollectionId(kBufferCollectionId),
                                 std::move(buffer_collection_token_client).TakeChannel()));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseBufferCollectionSuccess) {
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  mock_.ExpectReleaseBufferCollection([&](display::DriverBufferCollectionId buffer_collection_id) {
    EXPECT_EQ(kBufferCollectionId, buffer_collection_id);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.ReleaseBufferCollection(
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseBufferCollectionError) {
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  mock_.ExpectReleaseBufferCollection([&](display::DriverBufferCollectionId buffer_collection_id) {
    return zx::error(ZX_ERR_INTERNAL);
  });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.ReleaseBufferCollection(
                                 display::ToBanjoDriverBufferCollectionId(kBufferCollectionId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportImageSuccess) {
  static constexpr display::ImageMetadata kImageMetadata(
      {.width = 640, .height = 480, .tiling_type = display::ImageTilingType::kLinear});
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);
  static constexpr uint32_t kBufferIndex = 4242;
  static constexpr display::DriverImageId kImageId(84);

  mock_.ExpectImportImage([&](const display::ImageMetadata& image_metadata,
                              display::DriverBufferCollectionId buffer_collection_id,
                              uint32_t buffer_index) {
    EXPECT_EQ(kImageMetadata, image_metadata);
    EXPECT_EQ(kBufferCollectionId, buffer_collection_id);
    EXPECT_EQ(kBufferIndex, buffer_index);
    return zx::ok(kImageId);
  });

  static constexpr image_metadata_t kBanjoImageMetadata = kImageMetadata.ToBanjo();
  uint64_t banjo_image_handle = 0;
  EXPECT_OK(engine_banjo_.ImportImage(&kBanjoImageMetadata,
                                      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId),
                                      kBufferIndex, &banjo_image_handle));
  EXPECT_EQ(kImageId, display::ToDriverImageId(banjo_image_handle));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportImageError) {
  static constexpr display::ImageMetadata kImageMetadata(
      {.width = 640, .height = 480, .tiling_type = display::ImageTilingType::kLinear});
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);
  static constexpr uint32_t kBufferIndex = 4242;

  mock_.ExpectImportImage([&](const display::ImageMetadata& image_metadata,
                              display::DriverBufferCollectionId buffer_collection_id,
                              uint32_t buffer_index) { return zx::error(ZX_ERR_INTERNAL); });

  static constexpr image_metadata_t kBanjoImageMetadata = kImageMetadata.ToBanjo();
  uint64_t banjo_image_handle = 0;
  EXPECT_EQ(ZX_ERR_INTERNAL,
            engine_banjo_.ImportImage(&kBanjoImageMetadata,
                                      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId),
                                      kBufferIndex, &banjo_image_handle));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportImageForCaptureSuccess) {
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);
  static constexpr uint32_t kBufferIndex = 4242;
  static constexpr display::DriverCaptureImageId kCaptureImageId(84);

  mock_.ExpectImportImageForCapture(
      [&](display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) {
        EXPECT_EQ(kBufferCollectionId, buffer_collection_id);
        EXPECT_EQ(kBufferIndex, buffer_index);
        return zx::ok(kCaptureImageId);
      });

  uint64_t banjo_capture_image_handle = 0;
  EXPECT_OK(engine_banjo_.ImportImageForCapture(
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId), kBufferIndex,
      &banjo_capture_image_handle));
  EXPECT_EQ(kCaptureImageId, display::ToDriverCaptureImageId(banjo_capture_image_handle));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportImageForCaptureError) {
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);
  static constexpr uint32_t kBufferIndex = 4242;

  mock_.ExpectImportImageForCapture(
      [&](display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) {
        return zx::error(ZX_ERR_INTERNAL);
      });

  uint64_t banjo_capture_image_handle = 0;
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.ImportImageForCapture(
                                 display::ToBanjoDriverBufferCollectionId(kBufferCollectionId),
                                 kBufferIndex, &banjo_capture_image_handle));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseImageSuccess) {
  static constexpr display::DriverImageId kImageId(42);

  mock_.ExpectReleaseImage([&](display::DriverImageId buffer_collection_id) {
    EXPECT_EQ(kImageId, buffer_collection_id);
  });
  engine_banjo_.ReleaseImage(display::ToBanjoDriverImageId(kImageId));
}

TEST_F(DisplayEngineBanjoAdapterTest, CheckConfigurationSuccess) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::DriverLayer kLayer0({
      .display_destination = display::Rectangle({.x = 10, .y = 20, .width = 300, .height = 400}),
      .image_source = display::Rectangle({.x = 50, .y = 60, .width = 700, .height = 800}),
      .image_id = display::DriverImageId(4242),
      .image_metadata = display::ImageMetadata(
          {.width = 2048, .height = 1024, .tiling_type = display::ImageTilingType::kLinear}),
      .fallback_color = display::Color(
          {.format = display::PixelFormat::kB8G8R8A8,
           .bytes = std::initializer_list<uint8_t>{0x41, 0x42, 0x43, 0x44, 0, 0, 0, 0}}),
      .image_source_transformation = display::CoordinateTransformation::kIdentity,
  });

  static constexpr layer_t kBanjoLayer0 = kLayer0.ToBanjo();
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .layer_list = &kBanjoLayer0,
      .layer_count = 1,
  };

  mock_.ExpectCheckConfiguration(
      [&](display::DisplayId display_id, display::ModeId display_mode_id,
          cpp20::span<const display::DriverLayer> layers,
          cpp20::span<display::LayerCompositionOperations> layer_composition_operations) {
        EXPECT_EQ(kDisplayId, display_id);
        EXPECT_EQ(display::ModeId(1), display_mode_id);
        EXPECT_THAT(layers, ::testing::ElementsAre(kLayer0));
        EXPECT_THAT(layer_composition_operations,
                    ::testing::ElementsAre(display::LayerCompositionOperations::kNoOperations));
        return display::ConfigCheckResult::kOk;
      });

  std::array<layer_composition_operations_t, 1> banjo_layer_composition_operations;
  size_t banjo_layer_composition_operations_output_size = 0;
  EXPECT_EQ(display::ConfigCheckResult::kOk.ToBanjo(),
            engine_banjo_.CheckConfiguration(&kBanjoDisplayConfig, 1,
                                             banjo_layer_composition_operations.data(),
                                             banjo_layer_composition_operations.size(),
                                             &banjo_layer_composition_operations_output_size));
}

TEST_F(DisplayEngineBanjoAdapterTest, CheckConfigurationError) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::DriverLayer kLayer0({
      .display_destination = display::Rectangle({.x = 10, .y = 20, .width = 300, .height = 400}),
      .image_source = display::Rectangle({.x = 50, .y = 60, .width = 700, .height = 800}),
      .image_id = display::DriverImageId(4242),
      .image_metadata = display::ImageMetadata(
          {.width = 2048, .height = 1024, .tiling_type = display::ImageTilingType::kLinear}),
      .fallback_color = display::Color(
          {.format = display::PixelFormat::kB8G8R8A8,
           .bytes = std::initializer_list<uint8_t>{0x41, 0x42, 0x43, 0x44, 0, 0, 0, 0}}),
      .image_source_transformation = display::CoordinateTransformation::kIdentity,
  });

  static constexpr layer_t kBanjoLayer0 = kLayer0.ToBanjo();
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .layer_list = &kBanjoLayer0,
      .layer_count = 1,
  };

  mock_.ExpectCheckConfiguration(
      [&](display::DisplayId display_id, display::ModeId display_mode_id,
          cpp20::span<const display::DriverLayer> layers,
          cpp20::span<display::LayerCompositionOperations> layer_composition_operations) {
        return display::ConfigCheckResult::kUnsupportedDisplayModes;
      });

  std::array<layer_composition_operations_t, 1> banjo_layer_composition_operations;
  size_t banjo_layer_composition_operations_output_size = 0;
  EXPECT_EQ(display::ConfigCheckResult::kUnsupportedDisplayModes.ToBanjo(),
            engine_banjo_.CheckConfiguration(&kBanjoDisplayConfig, 1,
                                             banjo_layer_composition_operations.data(),
                                             banjo_layer_composition_operations.size(),
                                             &banjo_layer_composition_operations_output_size));
}

TEST_F(DisplayEngineBanjoAdapterTest, CheckConfigurationUnsupportedConfig) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::DriverLayer kLayer0({
      .display_destination = display::Rectangle({.x = 10, .y = 20, .width = 300, .height = 400}),
      .image_source = display::Rectangle({.x = 50, .y = 60, .width = 700, .height = 800}),
      .image_id = display::DriverImageId(4242),
      .image_metadata = display::ImageMetadata(
          {.width = 2048, .height = 1024, .tiling_type = display::ImageTilingType::kLinear}),
      .fallback_color = display::Color(
          {.format = display::PixelFormat::kB8G8R8A8,
           .bytes = std::initializer_list<uint8_t>{0x41, 0x42, 0x43, 0x44, 0, 0, 0, 0}}),
      .image_source_transformation = display::CoordinateTransformation::kIdentity,
  });
  static constexpr display::LayerCompositionOperations kLayer0Ops =
      display::LayerCompositionOperations::kFrameScale.WithSrcFrame();

  static constexpr layer_t kBanjoLayer0 = kLayer0.ToBanjo();
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .layer_list = &kBanjoLayer0,
      .layer_count = 1,
  };

  mock_.ExpectCheckConfiguration(
      [&](display::DisplayId display_id, display::ModeId display_mode_id,
          cpp20::span<const display::DriverLayer> layers,
          cpp20::span<display::LayerCompositionOperations> layer_composition_operations) {
        ZX_ASSERT(layer_composition_operations.size() >= 1);
        layer_composition_operations[0] = kLayer0Ops;
        return display::ConfigCheckResult::kUnsupportedConfig;
      });

  std::array<layer_composition_operations_t, 1> banjo_layer_composition_operations;
  size_t banjo_layer_composition_operations_output_size = 0;
  ASSERT_EQ(display::ConfigCheckResult::kUnsupportedConfig.ToBanjo(),
            engine_banjo_.CheckConfiguration(&kBanjoDisplayConfig, 1,
                                             banjo_layer_composition_operations.data(),
                                             banjo_layer_composition_operations.size(),
                                             &banjo_layer_composition_operations_output_size));
  ASSERT_EQ(1u, banjo_layer_composition_operations_output_size);
  EXPECT_EQ(kLayer0Ops.ToBanjo(), banjo_layer_composition_operations[0]);
}

TEST_F(DisplayEngineBanjoAdapterTest, ApplyConfiguration) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr display::DriverLayer kLayer0({
      .display_destination = display::Rectangle({.x = 10, .y = 20, .width = 300, .height = 400}),
      .image_source = display::Rectangle({.x = 50, .y = 60, .width = 700, .height = 800}),
      .image_id = display::DriverImageId(4242),
      .image_metadata = display::ImageMetadata(
          {.width = 2048, .height = 1024, .tiling_type = display::ImageTilingType::kLinear}),
      .fallback_color = display::Color(
          {.format = display::PixelFormat::kB8G8R8A8,
           .bytes = std::initializer_list<uint8_t>{0x41, 0x42, 0x43, 0x44, 0, 0, 0, 0}}),
      .image_source_transformation = display::CoordinateTransformation::kIdentity,
  });
  static constexpr display::ConfigStamp kConfigStamp(4242);

  static constexpr layer_t kBanjoLayer0 = kLayer0.ToBanjo();
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .layer_list = &kBanjoLayer0,
      .layer_count = 1,
  };
  static constexpr config_stamp_t kBanjoConfigStamp = display::ToBanjoConfigStamp(kConfigStamp);

  mock_.ExpectApplyConfiguration([&](display::DisplayId display_id, display::ModeId display_mode_id,
                                     cpp20::span<const display::DriverLayer> layers,
                                     display::ConfigStamp config_stamp) {
    EXPECT_EQ(kDisplayId, display_id);
    EXPECT_EQ(display::ModeId(1), display_mode_id);
    EXPECT_THAT(layers, ::testing::ElementsAre(kLayer0));
    EXPECT_EQ(kConfigStamp, config_stamp);
  });
  engine_banjo_.ApplyConfiguration(&kBanjoDisplayConfig, 1, &kBanjoConfigStamp);
}

TEST_F(DisplayEngineBanjoAdapterTest, SetBufferCollectionConstraintsSuccess) {
  static constexpr display::ImageBufferUsage kImageBufferUsage{
      .tiling_type = display::ImageTilingType::kLinear};
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  const image_buffer_usage_t kBanjoImageBufferUsage =
      display::ToBanjoImageBufferUsage(kImageBufferUsage);

  mock_.ExpectSetBufferCollectionConstraints(
      [&](const display::ImageBufferUsage& image_buffer_usage,
          display::DriverBufferCollectionId buffer_collection_id) {
        EXPECT_EQ(kImageBufferUsage, image_buffer_usage);
        EXPECT_EQ(kBufferCollectionId, buffer_collection_id);
        return zx::ok();
      });
  EXPECT_OK(engine_banjo_.SetBufferCollectionConstraints(
      &kBanjoImageBufferUsage, display::ToBanjoDriverBufferCollectionId(kBufferCollectionId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetBufferCollectionConstraintsError) {
  static constexpr display::ImageBufferUsage kImageBufferUsage{
      .tiling_type = display::ImageTilingType::kLinear};
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  const image_buffer_usage_t kBanjoImageBufferUsage =
      display::ToBanjoImageBufferUsage(kImageBufferUsage);

  mock_.ExpectSetBufferCollectionConstraints(
      [&](const display::ImageBufferUsage& image_buffer_usage,
          display::DriverBufferCollectionId buffer_collection_id) {
        return zx::error(ZX_ERR_INTERNAL);
      });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.SetBufferCollectionConstraints(
                                 &kBanjoImageBufferUsage,
                                 display::ToBanjoDriverBufferCollectionId(kBufferCollectionId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetDisplayPowerSuccess) {
  static constexpr display::DisplayId kDisplayId(42);

  mock_.ExpectSetDisplayPower([&](display::DisplayId display_id, bool power_on) {
    EXPECT_EQ(kDisplayId, display_id);
    EXPECT_EQ(true, power_on);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.SetDisplayPower(display::ToBanjoDisplayId(kDisplayId), true));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetDisplayPowerError) {
  static constexpr display::DisplayId kDisplayId(42);

  mock_.ExpectSetDisplayPower(
      [&](display::DisplayId display_id, bool power_on) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL,
            engine_banjo_.SetDisplayPower(display::ToBanjoDisplayId(kDisplayId), true));
}

TEST_F(DisplayEngineBanjoAdapterTest, IsCaptureSupported) {
  mock_.ExpectIsCaptureSupported([&]() { return false; });
  EXPECT_EQ(false, engine_banjo_.IsCaptureSupported());
}

TEST_F(DisplayEngineBanjoAdapterTest, StartCaptureSuccess) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectStartCapture([&](display::DriverCaptureImageId capture_image_id) {
    EXPECT_EQ(kCaptureImageId, capture_image_id);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.StartCapture(display::ToBanjoDriverCaptureImageId(kCaptureImageId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, StartCaptureError) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectStartCapture(
      [&](display::DriverCaptureImageId capture_image_id) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL,
            engine_banjo_.StartCapture(display::ToBanjoDriverCaptureImageId(kCaptureImageId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseCaptureSuccess) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectReleaseCapture([&](display::DriverCaptureImageId capture_image_id) {
    EXPECT_EQ(kCaptureImageId, capture_image_id);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.ReleaseCapture(display::ToBanjoDriverCaptureImageId(kCaptureImageId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseCaptureError) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectReleaseCapture(
      [&](display::DriverCaptureImageId capture_image_id) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL,
            engine_banjo_.ReleaseCapture(display::ToBanjoDriverCaptureImageId(kCaptureImageId)));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetMinimumRgbSuccess) {
  static constexpr uint8_t kMinimumRgb = 42;

  mock_.ExpectSetMinimumRgb([&](uint8_t minimum_rgb) {
    EXPECT_EQ(kMinimumRgb, minimum_rgb);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.SetMinimumRgb(kMinimumRgb));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetMinimumRgbError) {
  static constexpr uint8_t kMinimumRgb = 42;

  mock_.ExpectSetMinimumRgb([&](uint8_t minimum_rgb) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.SetMinimumRgb(kMinimumRgb));
}

}  // namespace

}  // namespace display
