// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-gpu-display/display-engine-banjo-adapter.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/virtio-gpu-display/display-engine-events-banjo.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace virtio_display {

DisplayEngineBanjoAdapter::DisplayEngineBanjoAdapter(DisplayEngine* engine,
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

void DisplayEngineBanjoAdapter::DisplayEngineRegisterDisplayEngineListener(
    const display_engine_listener_protocol_t* display_engine_listener) {
  ZX_DEBUG_ASSERT(display_engine_listener);
  engine_events_.RegisterDisplayEngineListener(display_engine_listener);
  if (display_engine_listener != nullptr) {
    engine_.OnCoordinatorConnected();
  }
}

void DisplayEngineBanjoAdapter::DisplayEngineDeregisterDisplayEngineListener() {
  engine_events_.RegisterDisplayEngineListener(nullptr);
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel banjo_buffer_collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token(
      std::move(banjo_buffer_collection_token));

  zx::result<> result = engine_.ImportBufferCollection(driver_buffer_collection_id,
                                                       std::move(buffer_collection_token));
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<> result = engine_.ReleaseBufferCollection(driver_buffer_collection_id);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineImportImage(
    const image_metadata_t* banjo_image_metadata, uint64_t banjo_driver_buffer_collection_id,
    uint32_t index, uint64_t* out_image_handle) {
  const display::ImageMetadata image_metadata(*banjo_image_metadata);
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<display::DriverImageId> result =
      engine_.ImportImage(image_metadata, driver_buffer_collection_id, index);
  if (result.is_error()) {
    return result.error_value();
  }
  *out_image_handle = display::ToBanjoDriverImageId(result.value());
  return ZX_OK;
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineImportImageForCapture(
    uint64_t banjo_driver_buffer_collection_id, uint32_t index, uint64_t* out_capture_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<display::DriverCaptureImageId> result =
      engine_.ImportImageForCapture(driver_buffer_collection_id, index);
  if (result.is_error()) {
    return result.error_value();
  }
  *out_capture_handle = display::ToBanjoDriverCaptureImageId(result.value());
  return ZX_OK;
}

void DisplayEngineBanjoAdapter::DisplayEngineReleaseImage(uint64_t banjo_image_handle) {
  const display::DriverImageId driver_image_id = display::ToDriverImageId(banjo_image_handle);
  engine_.ReleaseImage(driver_image_id);
}

config_check_result_t DisplayEngineBanjoAdapter::DisplayEngineCheckConfiguration(
    const display_config_t* banjo_display_configs_array, size_t banjo_display_configs_count,
    layer_composition_operations_t* out_layer_composition_operations_list,
    size_t out_layer_composition_operations_size, size_t* out_layer_composition_operations_actual) {
  cpp20::span<const display_config_t> banjo_display_configs(banjo_display_configs_array,
                                                            banjo_display_configs_count);
  cpp20::span<layer_composition_operations_t> out_layer_composition_operations(
      out_layer_composition_operations_list, out_layer_composition_operations_size);
  std::fill(out_layer_composition_operations.begin(), out_layer_composition_operations.end(), 0);

  if (out_layer_composition_operations_actual != nullptr) {
    *out_layer_composition_operations_actual = 0;
  }

  // The display coordinator currently uses zero-display configs to blank all
  // displays. We'll remove this eventually.
  if (banjo_display_configs.size() == 0) {
    return CONFIG_CHECK_RESULT_OK;
  }

  // This adapter does not support multiple-display operation. None of our
  // drivers supports this mode.
  if (banjo_display_configs.size() > 1) {
    ZX_DEBUG_ASSERT_MSG(false, "Multiple displays registered with the display coordinator");
    return CONFIG_CHECK_RESULT_TOO_MANY;
  }

  const display_config& banjo_display_config = banjo_display_configs[0];
  cpp20::span<const layer_t> banjo_layers(banjo_display_config.layer_list,
                                          banjo_display_config.layer_count);

  ZX_DEBUG_ASSERT(out_layer_composition_operations.size() >= banjo_layers.size());
  if (out_layer_composition_operations_actual != nullptr) {
    *out_layer_composition_operations_actual = banjo_layers.size();
  }

  // The display coordinator currently uses zero-display configs to blank a
  // display. We'll remove this eventually.
  if (banjo_layers.size() == 0) {
    return CONFIG_CHECK_RESULT_OK;
  }

  // This adapter does not currently support multi-layer configurations. This
  // restriction will be lifted in the near future.
  if (banjo_layers.size() > 1) {
    out_layer_composition_operations[0] = LAYER_COMPOSITION_OPERATIONS_MERGE_BASE;
    for (size_t i = 1; i < banjo_layers.size(); ++i) {
      out_layer_composition_operations[i] = LAYER_COMPOSITION_OPERATIONS_MERGE_SRC;
    }
    return CONFIG_CHECK_RESULT_OK;
  }

  // This adapter does not currently support color correction.
  if (banjo_display_config.cc_flags != 0) {
    out_layer_composition_operations[0] = LAYER_COMPOSITION_OPERATIONS_COLOR_CONVERSION;
    return CONFIG_CHECK_RESULT_OK;
  }

  if (!display::DriverLayer::IsValid(banjo_layers[0])) {
    // TODO(costan): Add an error code that indicates invalid input.
    return CONFIG_CHECK_RESULT_UNSUPPORTED_MODES;
  }
  display::DriverLayer layer(banjo_layers[0]);
  cpp20::span<const display::DriverLayer> layers(&layer, 1);

  return engine_.CheckConfiguration(display::ToDisplayId(banjo_display_config.display_id), layers,
                                    out_layer_composition_operations,
                                    out_layer_composition_operations_actual);
}

void DisplayEngineBanjoAdapter::DisplayEngineApplyConfiguration(
    const display_config_t* banjo_display_configs_array, size_t banjo_display_configs_count,
    const config_stamp_t* banjo_config_stamp) {
  cpp20::span<const display_config_t> banjo_display_configs(banjo_display_configs_array,
                                                            banjo_display_configs_count);

  // The display coordinator currently uses zero-display configs to blank all
  // displays. We'll remove this eventually.
  if (banjo_display_configs.size() == 0) {
    return;
  }

  // This adapter does not support multiple-display operation. None of our
  // drivers supports this mode.
  ZX_DEBUG_ASSERT_MSG(banjo_display_configs.size() == 1,
                      "Display coordinator applied rejected multi-display config");

  const display_config& banjo_display_config = banjo_display_configs[0];
  cpp20::span<const layer_t> banjo_layers(banjo_display_config.layer_list,
                                          banjo_display_config.layer_count);

  // The display coordinator currently uses zero-display configs to blank a
  // display. We'll remove this eventually.
  if (banjo_layers.size() == 0) {
    return;
  }

  // This adapter does not currently support multi-layer configurations. This
  // restriction will be lifted in the near future.
  ZX_DEBUG_ASSERT_MSG(banjo_layers.size() == 1,
                      "Display coordinator applied rejected multi-layer config");

  // This adapter does not currently support color correction.
  ZX_DEBUG_ASSERT_MSG(banjo_display_config.cc_flags == 0,
                      "Display coordinator applied rejected color-correction config");

  if (!display::DriverLayer::IsValid(banjo_layers[0])) {
    ZX_DEBUG_ASSERT_MSG(false, "Display coordinator applied rejected invalid layer config");
    return;
  }
  display::DriverLayer layer(banjo_layers[0]);
  cpp20::span<const display::DriverLayer> layers(&layer, 1);

  engine_.ApplyConfiguration(display::ToDisplayId(banjo_display_config.display_id), layers,
                             display::ToConfigStamp(*banjo_config_stamp));
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineSetBufferCollectionConstraints(
    const image_buffer_usage_t* banjo_image_buffer_usage,
    uint64_t banjo_driver_buffer_collection_id) {
  display::ImageBufferUsage image_buffer_usage =
      display::ToImageBufferUsage(*banjo_image_buffer_usage);
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<> result =
      engine_.SetBufferCollectionConstraints(image_buffer_usage, driver_buffer_collection_id);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineSetDisplayPower(uint64_t banjo_display_id,
                                                                    bool power_on) {
  const display::DisplayId display_id = display::ToDisplayId(banjo_display_id);
  zx::result<> result = engine_.SetDisplayPower(display_id, power_on);
  return result.status_value();
}

bool DisplayEngineBanjoAdapter::DisplayEngineIsCaptureSupported() {
  return engine_.IsCaptureSupported();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineStartCapture(uint64_t banjo_capture_handle) {
  const display::DriverCaptureImageId capture_image_id =
      display::ToDriverCaptureImageId(banjo_capture_handle);
  zx::result<> result = engine_.StartCapture(capture_image_id);
  return result.status_value();
}

zx_status_t DisplayEngineBanjoAdapter::DisplayEngineReleaseCapture(uint64_t banjo_capture_handle) {
  const display::DriverCaptureImageId capture_image_id =
      display::ToDriverCaptureImageId(banjo_capture_handle);
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

}  // namespace virtio_display
