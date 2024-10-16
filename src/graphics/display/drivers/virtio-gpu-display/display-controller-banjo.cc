// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-gpu-display/display-controller-banjo.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/virtio-gpu-display/display-coordinator-events-banjo.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types-cpp/image-metadata.h"

namespace virtio_display {

DisplayControllerBanjo::DisplayControllerBanjo(DisplayEngine* engine,
                                               DisplayCoordinatorEventsBanjo* coordinator_events)
    : engine_(*engine), coordinator_events_(*coordinator_events) {
  ZX_DEBUG_ASSERT(engine != nullptr);
  ZX_DEBUG_ASSERT(coordinator_events != nullptr);
}

DisplayControllerBanjo::~DisplayControllerBanjo() = default;

void DisplayControllerBanjo::DisplayEngineRegisterDisplayEngineListener(
    const display_engine_listener_protocol_t* display_engine_listener) {
  ZX_DEBUG_ASSERT(display_engine_listener);
  coordinator_events_.RegisterDisplayEngineListener(display_engine_listener);
  if (display_engine_listener != nullptr) {
    engine_.OnCoordinatorConnected();
  }
}

void DisplayControllerBanjo::DisplayEngineDeregisterDisplayEngineListener() {
  coordinator_events_.RegisterDisplayEngineListener(nullptr);
}

zx_status_t DisplayControllerBanjo::DisplayEngineImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel banjo_buffer_collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token(
      std::move(banjo_buffer_collection_token));

  zx::result<> result = engine_.ImportBufferCollection(driver_buffer_collection_id,
                                                       std::move(buffer_collection_token));
  return result.status_value();
}

zx_status_t DisplayControllerBanjo::DisplayEngineReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  zx::result<> result = engine_.ReleaseBufferCollection(driver_buffer_collection_id);
  return result.status_value();
}

zx_status_t DisplayControllerBanjo::DisplayEngineImportImage(
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

zx_status_t DisplayControllerBanjo::DisplayEngineImportImageForCapture(
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

void DisplayControllerBanjo::DisplayEngineReleaseImage(uint64_t banjo_image_handle) {
  const display::DriverImageId driver_image_id = display::ToDriverImageId(banjo_image_handle);
  engine_.ReleaseImage(driver_image_id);
}

config_check_result_t DisplayControllerBanjo::DisplayEngineCheckConfiguration(
    const display_config_t* banjo_display_configs, size_t banjo_display_configs_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t out_client_composition_opcodes_size, size_t* out_client_composition_opcodes_actual) {
  cpp20::span<const display_config_t> display_configs(banjo_display_configs,
                                                      banjo_display_configs_count);
  cpp20::span<client_composition_opcode_t> out_client_composition_opcodes(
      out_client_composition_opcodes_list, out_client_composition_opcodes_size);

  return engine_.CheckConfiguration(display_configs, out_client_composition_opcodes,
                                    out_client_composition_opcodes_actual);
}

void DisplayControllerBanjo::DisplayEngineApplyConfiguration(
    const display_config_t* banjo_display_configs, size_t banjo_display_configs_count,
    const config_stamp_t* banjo_config_stamp) {
  cpp20::span<const display_config_t> display_configs(banjo_display_configs,
                                                      banjo_display_configs_count);
  return engine_.ApplyConfiguration(display_configs, banjo_config_stamp);
}

zx_status_t DisplayControllerBanjo::DisplayEngineSetBufferCollectionConstraints(
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

zx_status_t DisplayControllerBanjo::DisplayEngineSetDisplayPower(uint64_t banjo_display_id,
                                                                 bool power_on) {
  const display::DisplayId display_id = display::ToDisplayId(banjo_display_id);
  zx::result<> result = engine_.SetDisplayPower(display_id, power_on);
  return result.status_value();
}

bool DisplayControllerBanjo::DisplayEngineIsCaptureSupported() {
  return engine_.IsCaptureSupported();
}

zx_status_t DisplayControllerBanjo::DisplayEngineStartCapture(uint64_t banjo_capture_handle) {
  const display::DriverCaptureImageId capture_image_id =
      display::ToDriverCaptureImageId(banjo_capture_handle);
  zx::result<> result = engine_.StartCapture(capture_image_id);
  return result.status_value();
}

zx_status_t DisplayControllerBanjo::DisplayEngineReleaseCapture(uint64_t banjo_capture_handle) {
  const display::DriverCaptureImageId capture_image_id =
      display::ToDriverCaptureImageId(banjo_capture_handle);
  zx::result<> result = engine_.ReleaseCapture(capture_image_id);
  return result.status_value();
}

zx_status_t DisplayControllerBanjo::DisplayEngineSetMinimumRgb(uint8_t minimum_rgb) {
  zx::result<> result = engine_.SetMinimumRgb(minimum_rgb);
  return result.status_value();
}

display_engine_protocol_t DisplayControllerBanjo::GetProtocol() {
  return {
      .ops = &display_engine_protocol_ops_,
      .ctx = this,
  };
}

}  // namespace virtio_display
