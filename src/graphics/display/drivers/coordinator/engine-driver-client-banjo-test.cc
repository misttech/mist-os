// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/engine-driver-client-banjo.h"

#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/zx/result.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/testing/mock-engine-banjo.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

namespace {

class EngineDriverClientBanjoTest : public ::testing::Test {
 public:
  void TearDown() override { mock_.CheckAllCallsReplayed(); }

 protected:
  fdf_testing::ScopedGlobalLogger logger_;

  testing::MockEngineBanjo mock_;
  const display_engine_protocol_t display_engine_protocol_ = mock_.GetProtocol();
  EngineDriverClientBanjo banjo_client_{&display_engine_protocol_};
};

TEST_F(EngineDriverClientBanjoTest, CompleteCoordinatorConnection) {
  display_engine_listener_protocol_t banjo_listener_proto = {};
  static constexpr engine_info_t kEngineInfo = {
      .max_layer_count = 3,
      .max_connected_display_count = 1,
      .is_capture_supported = true,
  };

  mock_.ExpectCompleteCoordinatorConnection(
      [&](const display_engine_listener_protocol_t* listener_protocol, engine_info_t* out_info) {
        *out_info = kEngineInfo;
      });

  auto [fidl_listener_client, fidl_listener_server] =
      fdf::Endpoints<fuchsia_hardware_display_engine::EngineListener>::Create();
  display::EngineInfo result = banjo_client_.CompleteCoordinatorConnection(
      banjo_listener_proto, std::move(fidl_listener_client));
  EXPECT_EQ(result.max_layer_count(), int{kEngineInfo.max_layer_count});
  EXPECT_EQ(result.max_connected_display_count(), int{kEngineInfo.max_connected_display_count});
  EXPECT_EQ(result.is_capture_supported(), kEngineInfo.is_capture_supported);
}

TEST_F(EngineDriverClientBanjoTest, UnsetListener) {
  mock_.ExpectUnsetListener([&]() {});
  banjo_client_.UnsetListener();
}

TEST_F(EngineDriverClientBanjoTest, ImportBufferCollectionSuccess) {
  static constexpr display::DriverBufferCollectionId kCollectionId(1);
  auto [token1, token2] = fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  mock_.ExpectImportBufferCollection([](uint64_t collection_id, zx::channel collection_token) {
    EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
    return ZX_OK;
  });

  EXPECT_OK(banjo_client_.ImportBufferCollection(kCollectionId, std::move(token1)));
}

TEST_F(EngineDriverClientBanjoTest, ImportBufferCollectionFailure) {
  static constexpr display::DriverBufferCollectionId kCollectionId(1);
  auto [token1, token2] = fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  mock_.ExpectImportBufferCollection([](uint64_t collection_id, zx::channel collection_token) {
    EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
    return ZX_ERR_INVALID_ARGS;
  });

  zx::result<> result = banjo_client_.ImportBufferCollection(kCollectionId, std::move(token1));
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, ReleaseBufferCollectionSuccess) {
  static constexpr display::DriverBufferCollectionId kCollectionId(1);

  mock_.ExpectReleaseBufferCollection([](uint64_t collection_id) {
    EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
    return ZX_OK;
  });

  EXPECT_OK(banjo_client_.ReleaseBufferCollection(kCollectionId));
}

TEST_F(EngineDriverClientBanjoTest, ReleaseBufferCollectionFailure) {
  static constexpr display::DriverBufferCollectionId kCollectionId(1);

  mock_.ExpectReleaseBufferCollection([](uint64_t collection_id) {
    EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
    return ZX_ERR_INVALID_ARGS;
  });

  zx::result<> result = banjo_client_.ReleaseBufferCollection(kCollectionId);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, ImportImageSuccess) {
  static constexpr display::DriverImageId kImageId(1);
  static constexpr display::DriverBufferCollectionId kCollectionId(2);
  static constexpr uint32_t kIndex = 3;
  static constexpr display::ImageMetadata kMetadata = {{
      .width = 1024,
      .height = 768,
      .tiling_type = display::ImageTilingType::kLinear,
  }};

  mock_.ExpectImportImage([](const image_metadata_t* image_metadata, uint64_t collection_id,
                             uint32_t index, uint64_t* out_image_handle) {
    EXPECT_EQ(image_metadata->dimensions.width, static_cast<uint32_t>(kMetadata.width()));
    EXPECT_EQ(image_metadata->dimensions.height, static_cast<uint32_t>(kMetadata.height()));
    EXPECT_EQ(display::ImageTilingType(image_metadata->tiling_type), kMetadata.tiling_type());
    EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
    EXPECT_EQ(index, kIndex);
    *out_image_handle = kImageId.ToBanjo();
    return ZX_OK;
  });

  zx::result<display::DriverImageId> result =
      banjo_client_.ImportImage(kMetadata, kCollectionId, kIndex);
  ASSERT_OK(result);
  EXPECT_EQ(result.value(), kImageId);
}

TEST_F(EngineDriverClientBanjoTest, ImportImageFailure) {
  static constexpr display::DriverBufferCollectionId kCollectionId(2);
  static constexpr uint32_t kIndex = 3;
  static constexpr display::ImageMetadata kMetadata = {{
      .width = 1024,
      .height = 768,
      .tiling_type = display::ImageTilingType::kLinear,
  }};

  mock_.ExpectImportImage([](const image_metadata_t* image_metadata, uint64_t collection_id,
                             uint32_t index, uint64_t* out_image_handle) {
    EXPECT_EQ(image_metadata->dimensions.width, static_cast<uint32_t>(kMetadata.width()));
    EXPECT_EQ(image_metadata->dimensions.height, static_cast<uint32_t>(kMetadata.height()));
    EXPECT_EQ(display::ImageTilingType(image_metadata->tiling_type), kMetadata.tiling_type());
    EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
    EXPECT_EQ(index, kIndex);
    return ZX_ERR_INVALID_ARGS;
  });

  zx::result<display::DriverImageId> result =
      banjo_client_.ImportImage(kMetadata, kCollectionId, kIndex);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, ImportImageForCaptureSuccess) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(1);
  static constexpr display::DriverBufferCollectionId kCollectionId(2);
  static constexpr uint32_t kIndex = 3;

  mock_.ExpectImportImageForCapture(
      [](uint64_t collection_id, uint32_t index, uint64_t* out_capture_handle) {
        EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
        EXPECT_EQ(index, kIndex);
        *out_capture_handle = kCaptureImageId.ToBanjo();
        return ZX_OK;
      });

  zx::result<display::DriverCaptureImageId> result =
      banjo_client_.ImportImageForCapture(kCollectionId, kIndex);
  ASSERT_OK(result);
  EXPECT_EQ(result.value(), kCaptureImageId);
}

TEST_F(EngineDriverClientBanjoTest, ImportImageForCaptureFailure) {
  static constexpr display::DriverBufferCollectionId kCollectionId(2);
  static constexpr uint32_t kIndex = 3;

  mock_.ExpectImportImageForCapture(
      [](uint64_t collection_id, uint32_t index, uint64_t* out_capture_handle) {
        EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
        EXPECT_EQ(index, kIndex);
        return ZX_ERR_INVALID_ARGS;
      });

  zx::result<display::DriverCaptureImageId> result =
      banjo_client_.ImportImageForCapture(kCollectionId, kIndex);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, ReleaseImage) {
  static constexpr display::DriverImageId kImageId(1);

  mock_.ExpectReleaseImage(
      [](uint64_t image_handle) { EXPECT_EQ(display::DriverImageId(image_handle), kImageId); });

  banjo_client_.ReleaseImage(kImageId);
}

TEST_F(EngineDriverClientBanjoTest, ReleaseCaptureSuccess) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(1);

  mock_.ExpectReleaseCapture([](uint64_t capture_handle) {
    EXPECT_EQ(display::DriverCaptureImageId(capture_handle), kCaptureImageId);
    return ZX_OK;
  });

  EXPECT_OK(banjo_client_.ReleaseCapture(kCaptureImageId));
}

TEST_F(EngineDriverClientBanjoTest, ReleaseCaptureFailure) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(1);

  mock_.ExpectReleaseCapture([](uint64_t capture_handle) {
    EXPECT_EQ(display::DriverCaptureImageId(capture_handle), kCaptureImageId);
    return ZX_ERR_INVALID_ARGS;
  });

  zx::result<> result = banjo_client_.ReleaseCapture(kCaptureImageId);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, SetBufferCollectionConstraintsSuccess) {
  static constexpr display::DriverBufferCollectionId kCollectionId(1);
  static constexpr display::ImageBufferUsage kUsage = {{
      .tiling_type = display::ImageTilingType::kLinear,
  }};

  mock_.ExpectSetBufferCollectionConstraints(
      [](const image_buffer_usage_t* usage, uint64_t collection_id) {
        ZX_ASSERT(usage != nullptr);
        EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
        EXPECT_EQ(display::ImageBufferUsage(*usage), kUsage);
        return ZX_OK;
      });

  EXPECT_OK(banjo_client_.SetBufferCollectionConstraints(kUsage, kCollectionId));
}

TEST_F(EngineDriverClientBanjoTest, SetBufferCollectionConstraintsFailure) {
  static constexpr display::DriverBufferCollectionId kCollectionId(1);
  static constexpr display::ImageBufferUsage kUsage = {{
      .tiling_type = display::ImageTilingType::kLinear,
  }};

  mock_.ExpectSetBufferCollectionConstraints(
      [](const image_buffer_usage_t* usage, uint64_t collection_id) {
        ZX_ASSERT(usage != nullptr);
        EXPECT_EQ(display::DriverBufferCollectionId(collection_id), kCollectionId);
        EXPECT_EQ(display::ImageBufferUsage(*usage), kUsage);
        return ZX_ERR_INVALID_ARGS;
      });

  zx::result<> result = banjo_client_.SetBufferCollectionConstraints(kUsage, kCollectionId);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, StartCaptureSuccess) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(1);

  mock_.ExpectStartCapture([](uint64_t capture_handle) {
    EXPECT_EQ(display::DriverCaptureImageId(capture_handle), kCaptureImageId);
    return ZX_OK;
  });

  EXPECT_OK(banjo_client_.StartCapture(kCaptureImageId));
}

TEST_F(EngineDriverClientBanjoTest, StartCaptureFailure) {
  static constexpr display::DriverCaptureImageId kCaptureImageId(1);

  mock_.ExpectStartCapture([](uint64_t capture_handle) {
    EXPECT_EQ(display::DriverCaptureImageId(capture_handle), kCaptureImageId);
    return ZX_ERR_INVALID_ARGS;
  });

  zx::result<> result = banjo_client_.StartCapture(kCaptureImageId);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, SetDisplayPowerSuccess) {
  static constexpr display::DisplayId kDisplayId(1);
  static constexpr bool kPowerOn = true;

  mock_.ExpectSetDisplayPower([](uint64_t display_id, bool power_on) {
    EXPECT_EQ(display::DisplayId(display_id), kDisplayId);
    EXPECT_EQ(power_on, kPowerOn);
    return ZX_OK;
  });

  EXPECT_OK(banjo_client_.SetDisplayPower(kDisplayId, kPowerOn));
}

TEST_F(EngineDriverClientBanjoTest, SetDisplayPowerFailure) {
  static constexpr display::DisplayId kDisplayId(1);
  static constexpr bool kPowerOn = true;

  mock_.ExpectSetDisplayPower([](uint64_t display_id, bool power_on) {
    EXPECT_EQ(display::DisplayId(display_id), kDisplayId);
    EXPECT_EQ(power_on, kPowerOn);
    return ZX_ERR_INVALID_ARGS;
  });

  zx::result<> result = banjo_client_.SetDisplayPower(kDisplayId, kPowerOn);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, SetMinimumRgbSuccess) {
  static constexpr uint8_t kMinimumRgb = 128;

  mock_.ExpectSetMinimumRgb([](uint8_t minimum_rgb) {
    EXPECT_EQ(minimum_rgb, kMinimumRgb);
    return ZX_OK;
  });

  EXPECT_OK(banjo_client_.SetMinimumRgb(kMinimumRgb));
}

TEST_F(EngineDriverClientBanjoTest, SetMinimumRgbFailure) {
  static constexpr uint8_t kMinimumRgb = 128;

  mock_.ExpectSetMinimumRgb([](uint8_t minimum_rgb) {
    EXPECT_EQ(minimum_rgb, kMinimumRgb);
    return ZX_ERR_INVALID_ARGS;
  });

  zx::result<> result = banjo_client_.SetMinimumRgb(kMinimumRgb);
  EXPECT_STATUS(result, zx::error(ZX_ERR_INVALID_ARGS));
}

TEST_F(EngineDriverClientBanjoTest, CheckConfiguration) {
  static constexpr layer_t kBanjoLayer = {
      .display_destination = {.x = 10, .y = 20, .width = 300, .height = 400},
      .image_source = {.x = 30, .y = 40, .width = 500, .height = 600},
      .image_handle = 4242,
      .image_metadata = {.dimensions = {.width = 700, .height = 800},
                         .tiling_type = IMAGE_TILING_TYPE_LINEAR},
      .fallback_color = {.format = static_cast<fuchsia_images2_pixel_format_enum_value_t>(
                             fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
                         .bytes = {0xff, 0, 0xff, 0xff, 0, 0, 0, 0}},
      .alpha_mode = ALPHA_PREMULTIPLIED,
      .alpha_layer_val = 0.25f,
      .image_source_transformation = COORDINATE_TRANSFORMATION_REFLECT_X,
  };
  static const layer_t* banjo_layers[] = {&kBanjoLayer};
  display_config_t banjo_config = {
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
              .coefficients = {{1.0f, 2.0f, 3.0f}, {4.0f, 5.0f, 6.0f}, {7.0f, 8.0f, 9.0f}},
              .postoffsets = {0.4f, 0.5f, 0.6f},
          },
      .layers_list = banjo_layers[0],
      .layers_count = 1,
  };

  mock_.ExpectCheckConfiguration([&](const display_config_t* display_config) {
    ZX_ASSERT(display_config != nullptr);

    EXPECT_EQ(display::DisplayId(display_config->display_id), display::DisplayId(1));

    EXPECT_EQ(display_config->mode_id, banjo_config.mode_id);
    EXPECT_EQ(display_config->timing.h_addressable, banjo_config.timing.h_addressable);
    EXPECT_EQ(display_config->timing.v_addressable, banjo_config.timing.v_addressable);

    EXPECT_THAT(display_config->color_conversion.preoffsets,
                ::testing::ElementsAre(0.1f, 0.2f, 0.3f));

    EXPECT_THAT(display_config->color_conversion.coefficients[0],
                ::testing::ElementsAre(1.0f, 2.0f, 3.0f));
    EXPECT_THAT(display_config->color_conversion.coefficients[1],
                ::testing::ElementsAre(4.0f, 5.0f, 6.0f));
    EXPECT_THAT(display_config->color_conversion.coefficients[2],
                ::testing::ElementsAre(7.0f, 8.0f, 9.0f));

    EXPECT_THAT(display_config->color_conversion.postoffsets,
                ::testing::ElementsAre(0.4f, 0.5f, 0.6f));

    ZX_ASSERT(display_config->layers_count == 1u);
    EXPECT_EQ(display::DriverLayer(display_config->layers_list[0]),
              display::DriverLayer(kBanjoLayer));

    return CONFIG_CHECK_RESULT_OK;
  });

  auto result = banjo_client_.CheckConfiguration(&banjo_config);
  EXPECT_EQ(result, display::ConfigCheckResult::kOk);
}

TEST_F(EngineDriverClientBanjoTest, ApplyConfiguration) {
  static constexpr layer_t kBanjoLayer = {
      .display_destination = {.x = 10, .y = 20, .width = 300, .height = 400},
      .image_source = {.x = 30, .y = 40, .width = 500, .height = 600},
      .image_handle = 4242,
      .image_metadata = {.dimensions = {.width = 700, .height = 800},
                         .tiling_type = IMAGE_TILING_TYPE_LINEAR},
      .fallback_color = {.format = static_cast<fuchsia_images2_pixel_format_enum_value_t>(
                             fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
                         .bytes = {0xff, 0, 0xff, 0xff, 0, 0, 0, 0}},
      .alpha_mode = ALPHA_PREMULTIPLIED,
      .alpha_layer_val = 0.25f,
      .image_source_transformation = COORDINATE_TRANSFORMATION_REFLECT_X,
  };
  static const layer_t* banjo_layers[] = {&kBanjoLayer};
  display_config_t banjo_config = {
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
              .coefficients = {{1.0f, 2.0f, 3.0f}, {4.0f, 5.0f, 6.0f}, {7.0f, 8.0f, 9.0f}},
              .postoffsets = {0.4f, 0.5f, 0.6f},
          },
      .layers_list = banjo_layers[0],
      .layers_count = 1,
  };
  static constexpr display::DriverConfigStamp kConfigStamp(42);

  mock_.ExpectApplyConfiguration(
      [&](const display_config_t* display_config, const config_stamp_t* config_stamp) {
        ZX_ASSERT(display_config != nullptr);
        ZX_ASSERT(config_stamp != nullptr);

        EXPECT_EQ(display::DisplayId(display_config->display_id), display::DisplayId(1));

        EXPECT_EQ(display_config->mode_id, banjo_config.mode_id);
        EXPECT_EQ(display_config->timing.h_addressable, banjo_config.timing.h_addressable);
        EXPECT_EQ(display_config->timing.v_addressable, banjo_config.timing.v_addressable);

        EXPECT_THAT(display_config->color_conversion.preoffsets,
                    ::testing::ElementsAre(0.1f, 0.2f, 0.3f));

        EXPECT_THAT(display_config->color_conversion.coefficients[0],
                    ::testing::ElementsAre(1.0f, 2.0f, 3.0f));
        EXPECT_THAT(display_config->color_conversion.coefficients[1],
                    ::testing::ElementsAre(4.0f, 5.0f, 6.0f));
        EXPECT_THAT(display_config->color_conversion.coefficients[2],
                    ::testing::ElementsAre(7.0f, 8.0f, 9.0f));

        EXPECT_THAT(display_config->color_conversion.postoffsets,
                    ::testing::ElementsAre(0.4f, 0.5f, 0.6f));

        ASSERT_EQ(display_config->layers_count, 1u);
        EXPECT_EQ(display::DriverLayer(display_config->layers_list[0]),
                  display::DriverLayer(kBanjoLayer));
        EXPECT_EQ(display::DriverConfigStamp(*config_stamp), kConfigStamp);
      });

  banjo_client_.ApplyConfiguration(&banjo_config, kConfigStamp);
}

}  // namespace

}  // namespace display_coordinator
