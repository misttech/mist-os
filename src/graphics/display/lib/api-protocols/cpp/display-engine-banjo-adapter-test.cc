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
#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-protocols/cpp/mock-banjo-display-engine-listener.h"
#include "src/graphics/display/lib/api-protocols/cpp/mock-display-engine.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/coordinate-transformation.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/engine-info.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"
#include "src/graphics/display/lib/api-types/cpp/rectangle.h"
#include "src/lib/testing/predicates/status.h"

namespace display {

namespace {

class DisplayEngineBanjoAdapterTest : public ::testing::Test {
 public:
  void TearDown() override { mock_.CheckAllCallsReplayed(); }

 protected:
  display::testing::MockDisplayEngine mock_;
  display::DisplayEngineEventsBanjo engine_events_banjo_;
  display::DisplayEngineBanjoAdapter banjo_adapter_{&mock_, &engine_events_banjo_};
  const display_engine_protocol_t display_engine_protocol_ = banjo_adapter_.GetProtocol();
  ddk::DisplayEngineProtocolClient engine_banjo_{&display_engine_protocol_};
};

TEST_F(DisplayEngineBanjoAdapterTest, CompleteCoordinatorConnection) {
  static constexpr display::EngineInfo kEngineInfo({
      .max_layer_count = 4,
      .max_connected_display_count = 1,
      .is_capture_supported = false,
  });

  testing::MockBanjoDisplayEngineListener mock_listener;
  mock_.ExpectCompleteCoordinatorConnection([&] { return kEngineInfo; });

  engine_info_t banjo_engine_info;
  engine_banjo_.CompleteCoordinatorConnection(mock_listener.GetProtocol().ctx,
                                              mock_listener.GetProtocol().ops, &banjo_engine_info);
  EXPECT_EQ(kEngineInfo, display::EngineInfo::From(banjo_engine_info));

  mock_listener.CheckAllCallsReplayed();
}

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
      kBufferCollectionId.ToBanjo(), std::move(buffer_collection_token_client).TakeChannel()));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportBufferCollectionEngineError) {
  auto [buffer_collection_token_client, buffer_collection_token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  mock_.ExpectImportBufferCollection(
      [&](display::DriverBufferCollectionId buffer_collection_id,
          fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) {
        return zx::error(ZX_ERR_INTERNAL);
      });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.ImportBufferCollection(
                                 kBufferCollectionId.ToBanjo(),
                                 std::move(buffer_collection_token_client).TakeChannel()));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseBufferCollectionSuccess) {
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  mock_.ExpectReleaseBufferCollection([&](display::DriverBufferCollectionId buffer_collection_id) {
    EXPECT_EQ(kBufferCollectionId, buffer_collection_id);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.ReleaseBufferCollection(kBufferCollectionId.ToBanjo()));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseBufferCollectionEnginerror) {
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  mock_.ExpectReleaseBufferCollection([&](display::DriverBufferCollectionId buffer_collection_id) {
    return zx::error(ZX_ERR_INTERNAL);
  });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.ReleaseBufferCollection(kBufferCollectionId.ToBanjo()));
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
  EXPECT_OK(engine_banjo_.ImportImage(&kBanjoImageMetadata, kBufferCollectionId.ToBanjo(),
                                      kBufferIndex, &banjo_image_handle));
  EXPECT_EQ(kImageId, display::DriverImageId(banjo_image_handle));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportImageEngineError) {
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
            engine_banjo_.ImportImage(&kBanjoImageMetadata, kBufferCollectionId.ToBanjo(),
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
  EXPECT_OK(engine_banjo_.ImportImageForCapture(kBufferCollectionId.ToBanjo(), kBufferIndex,
                                                &banjo_capture_image_handle));
  EXPECT_EQ(kCaptureImageId, display::DriverCaptureImageId(banjo_capture_image_handle));
}

TEST_F(DisplayEngineBanjoAdapterTest, ImportImageForCaptureEngineError) {
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);
  static constexpr uint32_t kBufferIndex = 4242;

  mock_.ExpectImportImageForCapture(
      [&](display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) {
        return zx::error(ZX_ERR_INTERNAL);
      });

  uint64_t banjo_capture_image_handle = 0;
  EXPECT_EQ(ZX_ERR_INTERNAL,
            engine_banjo_.ImportImageForCapture(kBufferCollectionId.ToBanjo(), kBufferIndex,
                                                &banjo_capture_image_handle));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseImageSuccess) {
  static constexpr display::DriverImageId kImageId(42);

  mock_.ExpectReleaseImage([&](display::DriverImageId buffer_collection_id) {
    EXPECT_EQ(kImageId, buffer_collection_id);
  });
  engine_banjo_.ReleaseImage(kImageId.ToBanjo());
}

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

TEST_F(DisplayEngineBanjoAdapterTest, CheckConfigurationSingleLayerSuccess) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::DriverLayer, 1> kLayers = {CreateValidLayerWithSeed(0)};

  static constexpr std::array<layer_t, 1> kBanjoLayers = {kLayers[0].ToBanjo()};
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = kDisplayId.ToBanjo(),
      .layers_list = kBanjoLayers.data(),
      .layers_count = kBanjoLayers.size(),
  };

  mock_.ExpectCheckConfiguration([&](display::DisplayId display_id, display::ModeId display_mode_id,
                                     cpp20::span<const display::DriverLayer> layers) {
    EXPECT_EQ(kDisplayId, display_id);
    EXPECT_EQ(display::ModeId(1), display_mode_id);
    EXPECT_THAT(layers, ::testing::ElementsAreArray(kLayers));
    return display::ConfigCheckResult::kOk;
  });

  EXPECT_EQ(display::ConfigCheckResult::kOk.ToBanjo(),
            engine_banjo_.CheckConfiguration(&kBanjoDisplayConfig));
}

TEST_F(DisplayEngineBanjoAdapterTest, CheckConfigurationMultiLayerSuccess) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::DriverLayer, 4> kLayers = {
      CreateValidLayerWithSeed(0), CreateValidLayerWithSeed(1), CreateValidLayerWithSeed(2),
      CreateValidLayerWithSeed(3)};

  static constexpr std::array<layer_t, 4> banjo_layers = {
      kLayers[0].ToBanjo(), kLayers[1].ToBanjo(), kLayers[2].ToBanjo(), kLayers[3].ToBanjo()};
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = kDisplayId.ToBanjo(),
      .layers_list = banjo_layers.data(),
      .layers_count = banjo_layers.size(),
  };

  mock_.ExpectCheckConfiguration([&](display::DisplayId display_id, display::ModeId display_mode_id,
                                     cpp20::span<const display::DriverLayer> layers) {
    EXPECT_EQ(kDisplayId, display_id);
    EXPECT_EQ(display::ModeId(1), display_mode_id);
    EXPECT_THAT(layers, ::testing::ElementsAreArray(kLayers));
    return display::ConfigCheckResult::kOk;
  });

  EXPECT_EQ(display::ConfigCheckResult::kOk.ToBanjo(),
            engine_banjo_.CheckConfiguration(&kBanjoDisplayConfig));
}

TEST_F(DisplayEngineBanjoAdapterTest, CheckConfigurationAdapterErrorLayerCountPastLimit) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr int kLayerCount = display::EngineInfo::kMaxAllowedMaxLayerCount + 1;

  std::vector<layer_t> banjo_layers;
  banjo_layers.reserve(kLayerCount);
  for (int i = 0; i < kLayerCount; ++i) {
    banjo_layers.emplace_back(CreateValidLayerWithSeed(i).ToBanjo());
  }

  const display_config_t kBanjoDisplayConfig = {
      .display_id = kDisplayId.ToBanjo(),
      .layers_list = banjo_layers.data(),
      .layers_count = banjo_layers.size(),
  };

  // The adapter is expected to reject the CheckConfiguration() call without
  // invoking the engine driver. So, the mock driver should not receive any
  // calls.

  EXPECT_EQ(display::ConfigCheckResult::kUnsupportedConfig.ToBanjo(),
            engine_banjo_.CheckConfiguration(&kBanjoDisplayConfig));
}

TEST_F(DisplayEngineBanjoAdapterTest, CheckConfigurationEngineError) {
  static constexpr display::DisplayId kDisplayId(42);

  static constexpr std::array<layer_t, 1> kBanjoLayers = {CreateValidLayerWithSeed(0).ToBanjo()};
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = kDisplayId.ToBanjo(),
      .layers_list = kBanjoLayers.data(),
      .layers_count = kBanjoLayers.size(),
  };

  mock_.ExpectCheckConfiguration([&](display::DisplayId display_id, display::ModeId display_mode_id,
                                     cpp20::span<const display::DriverLayer> layers) {
    return display::ConfigCheckResult::kUnsupportedDisplayModes;
  });

  EXPECT_EQ(display::ConfigCheckResult::kUnsupportedDisplayModes.ToBanjo(),
            engine_banjo_.CheckConfiguration(&kBanjoDisplayConfig));
}

TEST_F(DisplayEngineBanjoAdapterTest, ApplyConfigurationSingleLayer) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::DriverLayer, 1> kLayers = {CreateValidLayerWithSeed(0)};
  static constexpr display::DriverConfigStamp kConfigStamp(4242);

  static constexpr std::array<layer_t, 1> kBanjoLayers = {kLayers[0].ToBanjo()};
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = kDisplayId.ToBanjo(),
      .layers_list = kBanjoLayers.data(),
      .layers_count = kBanjoLayers.size(),
  };
  static constexpr config_stamp_t kBanjoConfigStamp = kConfigStamp.ToBanjo();

  mock_.ExpectApplyConfiguration([&](display::DisplayId display_id, display::ModeId display_mode_id,
                                     cpp20::span<const display::DriverLayer> layers,
                                     display::DriverConfigStamp config_stamp) {
    EXPECT_EQ(kDisplayId, display_id);
    EXPECT_EQ(display::ModeId(1), display_mode_id);
    EXPECT_THAT(layers, ::testing::ElementsAreArray(kLayers));
    EXPECT_EQ(kConfigStamp, config_stamp);
  });
  engine_banjo_.ApplyConfiguration(&kBanjoDisplayConfig, &kBanjoConfigStamp);
}

TEST_F(DisplayEngineBanjoAdapterTest, ApplyConfigurationMultiLayer) {
  static constexpr display::DisplayId kDisplayId(42);
  static constexpr std::array<display::DriverLayer, 4> kLayers = {
      CreateValidLayerWithSeed(0), CreateValidLayerWithSeed(1), CreateValidLayerWithSeed(2),
      CreateValidLayerWithSeed(3)};
  static constexpr display::DriverConfigStamp kConfigStamp(4242);

  static constexpr std::array<layer_t, 4> banjo_layers = {
      kLayers[0].ToBanjo(), kLayers[1].ToBanjo(), kLayers[2].ToBanjo(), kLayers[3].ToBanjo()};
  static constexpr display_config_t kBanjoDisplayConfig = {
      .display_id = kDisplayId.ToBanjo(),
      .layers_list = banjo_layers.data(),
      .layers_count = banjo_layers.size(),
  };
  static constexpr config_stamp_t kBanjoConfigStamp = kConfigStamp.ToBanjo();

  mock_.ExpectApplyConfiguration([&](display::DisplayId display_id, display::ModeId display_mode_id,
                                     cpp20::span<const display::DriverLayer> layers,
                                     display::DriverConfigStamp config_stamp) {
    EXPECT_EQ(kDisplayId, display_id);
    EXPECT_EQ(display::ModeId(1), display_mode_id);
    EXPECT_THAT(layers, ::testing::ElementsAreArray(kLayers));
    EXPECT_EQ(kConfigStamp, config_stamp);
  });
  engine_banjo_.ApplyConfiguration(&kBanjoDisplayConfig, &kBanjoConfigStamp);
}

TEST_F(DisplayEngineBanjoAdapterTest, SetBufferCollectionConstraintsSuccess) {
  static constexpr display::ImageBufferUsage kImageBufferUsage(
      {.tiling_type = display::ImageTilingType::kLinear});
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  const image_buffer_usage_t kBanjoImageBufferUsage = kImageBufferUsage.ToBanjo();

  mock_.ExpectSetBufferCollectionConstraints(
      [&](const display::ImageBufferUsage& image_buffer_usage,
          display::DriverBufferCollectionId buffer_collection_id) {
        EXPECT_EQ(kImageBufferUsage, image_buffer_usage);
        EXPECT_EQ(kBufferCollectionId, buffer_collection_id);
        return zx::ok();
      });
  EXPECT_OK(engine_banjo_.SetBufferCollectionConstraints(&kBanjoImageBufferUsage,
                                                         kBufferCollectionId.ToBanjo()));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetBufferCollectionConstraintsEngineError) {
  static constexpr display::ImageBufferUsage kImageBufferUsage(
      {.tiling_type = display::ImageTilingType::kLinear});
  static constexpr display::DriverBufferCollectionId kBufferCollectionId(42);

  const image_buffer_usage_t kBanjoImageBufferUsage = kImageBufferUsage.ToBanjo();

  mock_.ExpectSetBufferCollectionConstraints(
      [&](const display::ImageBufferUsage& image_buffer_usage,
          display::DriverBufferCollectionId buffer_collection_id) {
        return zx::error(ZX_ERR_INTERNAL);
      });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.SetBufferCollectionConstraints(
                                 &kBanjoImageBufferUsage, kBufferCollectionId.ToBanjo()));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetDisplayPowerSuccess) {
  static constexpr display::DisplayId kDisplayId(42);

  mock_.ExpectSetDisplayPower([&](display::DisplayId display_id, bool power_on) {
    EXPECT_EQ(kDisplayId, display_id);
    EXPECT_EQ(true, power_on);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.SetDisplayPower(kDisplayId.ToBanjo(), true));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetDisplayPowerEngineError) {
  static constexpr display::DisplayId kDisplayId(42);

  mock_.ExpectSetDisplayPower(
      [&](display::DisplayId display_id, bool power_on) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.SetDisplayPower(kDisplayId.ToBanjo(), true));
}

TEST_F(DisplayEngineBanjoAdapterTest, StartCaptureSuccess) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectStartCapture([&](display::DriverCaptureImageId capture_image_id) {
    EXPECT_EQ(kCaptureImageId, capture_image_id);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.StartCapture(kCaptureImageId.ToBanjo()));
}

TEST_F(DisplayEngineBanjoAdapterTest, StartCaptureEngineError) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectStartCapture(
      [&](display::DriverCaptureImageId capture_image_id) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.StartCapture(kCaptureImageId.ToBanjo()));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseCaptureSuccess) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectReleaseCapture([&](display::DriverCaptureImageId capture_image_id) {
    EXPECT_EQ(kCaptureImageId, capture_image_id);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.ReleaseCapture(kCaptureImageId.ToBanjo()));
}

TEST_F(DisplayEngineBanjoAdapterTest, ReleaseCaptureEngineError) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(42);

  mock_.ExpectReleaseCapture(
      [&](display::DriverCaptureImageId capture_image_id) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.ReleaseCapture(kCaptureImageId.ToBanjo()));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetMinimumRgbSuccess) {
  static constexpr uint8_t kMinimumRgb = 42;

  mock_.ExpectSetMinimumRgb([&](uint8_t minimum_rgb) {
    EXPECT_EQ(kMinimumRgb, minimum_rgb);
    return zx::ok();
  });
  EXPECT_OK(engine_banjo_.SetMinimumRgb(kMinimumRgb));
}

TEST_F(DisplayEngineBanjoAdapterTest, SetMinimumRgbEngineError) {
  static constexpr uint8_t kMinimumRgb = 42;

  mock_.ExpectSetMinimumRgb([&](uint8_t minimum_rgb) { return zx::error(ZX_ERR_INTERNAL); });
  EXPECT_EQ(ZX_ERR_INTERNAL, engine_banjo_.SetMinimumRgb(kMinimumRgb));
}

}  // namespace

}  // namespace display
