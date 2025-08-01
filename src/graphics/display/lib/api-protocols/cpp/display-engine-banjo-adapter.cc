// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-banjo-adapter.h"

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <algorithm>
#include <array>
#include <cstdint>
#include <utility>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-banjo.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"
#include "src/graphics/display/lib/api-protocols/cpp/inplace-vector.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace display {

namespace {

bool IsIdentityColorConversion(const color_conversion_t& color_conversion) {
  if (!std::equal(std::begin(color_conversion.preoffsets), std::end(color_conversion.preoffsets),
                  std::array<float, 3>{0.0f, 0.0f, 0.0f}.begin())) {
    return false;
  }
  if (!std::equal(std::begin(color_conversion.coefficients[0]),
                  std::end(color_conversion.coefficients[0]),
                  std::array<float, 3>{1.0f, 0.0f, 0.0f}.begin())) {
    return false;
  }
  if (!std::equal(std::begin(color_conversion.coefficients[1]),
                  std::end(color_conversion.coefficients[1]),
                  std::array<float, 3>{0.0f, 1.0f, 0.0f}.begin())) {
    return false;
  }
  if (!std::equal(std::begin(color_conversion.coefficients[2]),
                  std::end(color_conversion.coefficients[2]),
                  std::array<float, 3>{0.0f, 0.0f, 1.0f}.begin())) {
    return false;
  }
  if (!std::equal(std::begin(color_conversion.postoffsets), std::end(color_conversion.postoffsets),
                  std::array<float, 3>{0.0f, 0.0f, 0.0f}.begin())) {
    return false;
  }

  return true;
}

}  // namespace

DisplayEngineBanjoAdapter::DisplayEngineBanjoAdapter(DisplayEngineInterface* engine,
                                                     DisplayEngineEventsBanjo* engine_events)
    : engine_(*engine),
      engine_events_(*engine_events),
      banjo_server_(ZX_PROTOCOL_DISPLAY_ENGINE, GetProtocol().ctx, GetProtocol().ops) {
  ZX_DEBUG_ASSERT(engine != nullptr);
  ZX_DEBUG_ASSERT(engine_events != nullptr);
}

DisplayEngineBanjoAdapter::~DisplayEngineBanjoAdapter() = default;

compat::DeviceServer::BanjoConfig DisplayEngineBanjoAdapter::CreateBanjoConfig() {
  compat::DeviceServer::BanjoConfig banjo_config;
  banjo_config.callbacks[ZX_PROTOCOL_DISPLAY_ENGINE] = banjo_server_.callback();
  return banjo_config;
}

void DisplayEngineBanjoAdapter::DisplayEngineCompleteCoordinatorConnection(
    const display_engine_listener_protocol_t* display_engine_listener,
    engine_info_t* out_banjo_engine_info) {
  ZX_DEBUG_ASSERT(display_engine_listener != nullptr);
  ZX_DEBUG_ASSERT(out_banjo_engine_info != nullptr);

  engine_events_.SetListener(display_engine_listener);
  const EngineInfo engine_info = engine_.CompleteCoordinatorConnection();
  *out_banjo_engine_info = engine_info.ToBanjo();
}

void DisplayEngineBanjoAdapter::DisplayEngineUnsetListener() {
  engine_events_.SetListener(nullptr);
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel banjo_buffer_collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::DriverBufferCollectionId(banjo_driver_buffer_collection_id);
  fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token(
      std::move(banjo_buffer_collection_token));

  zx::result<> result = engine_.ImportBufferCollection(driver_buffer_collection_id,
                                                       std::move(buffer_collection_token));
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::DriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<> result = engine_.ReleaseBufferCollection(driver_buffer_collection_id);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineImportImage(
    const image_metadata_t* banjo_image_metadata, uint64_t banjo_driver_buffer_collection_id,
    uint32_t index, uint64_t* out_image_handle) {
  const display::ImageMetadata image_metadata(*banjo_image_metadata);
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::DriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<display::DriverImageId> result =
      engine_.ImportImage(image_metadata, driver_buffer_collection_id, index);
  if (result.is_error()) {
    return result.error_value();
  }
  *out_image_handle = result.value().ToBanjo();
  return ZX_OK;
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineImportImageForCapture(
    uint64_t banjo_driver_buffer_collection_id, uint32_t index, uint64_t* out_capture_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::DriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<display::DriverCaptureImageId> result =
      engine_.ImportImageForCapture(driver_buffer_collection_id, index);
  if (result.is_error()) {
    return result.error_value();
  }
  *out_capture_handle = result.value().ToBanjo();
  return ZX_OK;
}

void DisplayEngineBanjoAdapter::DisplayEngineReleaseImage(uint64_t banjo_image_handle) {
  const display::DriverImageId driver_image_id = display::DriverImageId(banjo_image_handle);
  engine_.ReleaseImage(driver_image_id);
}

config_check_result_t DisplayEngineBanjoAdapter::DisplayEngineCheckConfiguration(
    const display_config_t* banjo_display_config) {
  cpp20::span<const layer_t> banjo_layers(banjo_display_config->layers_list,
                                          banjo_display_config->layers_count);

  ZX_DEBUG_ASSERT_MSG(!banjo_layers.empty(),
                      "Display Coordinator checked empty config (zero layers)");

  if (banjo_layers.size() > display::EngineInfo::kMaxAllowedMaxLayerCount) {
    return display::ConfigCheckResult::kUnsupportedConfig.ToBanjo();
  }

  // This adapter does not currently support non-identity color correction.
  if (!IsIdentityColorConversion(banjo_display_config->color_conversion)) {
    return display::ConfigCheckResult::kUnsupportedConfig.ToBanjo();
  }

  internal::InplaceVector<display::DriverLayer, display::EngineInfo::kMaxAllowedMaxLayerCount>
      layers;
  for (const auto& banjo_layer : banjo_layers) {
    ZX_DEBUG_ASSERT(display::DriverLayer::IsValid(banjo_layer));
    layers.emplace_back(banjo_layer);
  }

  display::ConfigCheckResult config_check_result = engine_.CheckConfiguration(
      display::DisplayId(banjo_display_config->display_id), display::ModeId(1), layers);
  return config_check_result.ToBanjo();
}

void DisplayEngineBanjoAdapter::DisplayEngineApplyConfiguration(
    const display_config_t* banjo_display_config, const config_stamp_t* banjo_config_stamp) {
  cpp20::span<const layer_t> banjo_layers(banjo_display_config->layers_list,
                                          banjo_display_config->layers_count);

  // The display coordinator currently uses zero-display configs to blank a
  // display. We'll remove this eventually.
  if (banjo_layers.size() == 0) {
    return;
  }

  ZX_DEBUG_ASSERT_MSG(banjo_layers.size() <= display::EngineInfo::kMaxAllowedMaxLayerCount,
                      "Display coordinator applied rejected config with too many layers");

  // This adapter does not currently support non-identity color correction.
  ZX_DEBUG_ASSERT_MSG(IsIdentityColorConversion(banjo_display_config->color_conversion),
                      "Display coordinator applied rejected non-identity color-correction config");

  internal::InplaceVector<display::DriverLayer, display::EngineInfo::kMaxAllowedMaxLayerCount>
      layers;
  for (const auto& banjo_layer : banjo_layers) {
    ZX_DEBUG_ASSERT(display::DriverLayer::IsValid(banjo_layer));
    layers.emplace_back(banjo_layer);
  }

  engine_.ApplyConfiguration(display::DisplayId(banjo_display_config->display_id),
                             display::ModeId(1), layers,
                             display::DriverConfigStamp(*banjo_config_stamp));
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineSetBufferCollectionConstraints(
    const image_buffer_usage_t* banjo_image_buffer_usage,
    uint64_t banjo_driver_buffer_collection_id) {
  display::ImageBufferUsage image_buffer_usage(*banjo_image_buffer_usage);
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::DriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<> result =
      engine_.SetBufferCollectionConstraints(image_buffer_usage, driver_buffer_collection_id);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineSetDisplayPower(uint64_t banjo_display_id,
                                                                    bool power_on) {
  const display::DisplayId display_id = display::DisplayId(banjo_display_id);
  zx::result<> result = engine_.SetDisplayPower(display_id, power_on);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineStartCapture(uint64_t banjo_capture_handle) {
  const display::DriverCaptureImageId capture_image_id =
      display::DriverCaptureImageId(banjo_capture_handle);
  zx::result<> result = engine_.StartCapture(capture_image_id);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineReleaseCapture(uint64_t banjo_capture_handle) {
  const display::DriverCaptureImageId capture_image_id =
      display::DriverCaptureImageId(banjo_capture_handle);
  zx::result<> result = engine_.ReleaseCapture(capture_image_id);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineSetMinimumRgb(uint8_t minimum_rgb) {
  zx::result<> result = engine_.SetMinimumRgb(minimum_rgb);
  return result.status_value();
}

display_engine_protocol_t DisplayEngineBanjoAdapter::GetProtocol() {
  return {
      .ops = &display_engine_protocol_ops_,
      .ctx = this,
  };
}

}  // namespace display
