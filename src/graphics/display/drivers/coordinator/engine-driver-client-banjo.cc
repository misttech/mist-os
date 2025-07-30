// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/engine-driver-client-banjo.h"

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstdint>

namespace display_coordinator {

EngineDriverClientBanjo::EngineDriverClientBanjo(ddk::DisplayEngineProtocolClient banjo_engine)
    : banjo_engine_(banjo_engine) {
  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
}

EngineDriverClientBanjo::~EngineDriverClientBanjo() = default;

void EngineDriverClientBanjo::ReleaseImage(display::DriverImageId driver_image_id) {
  banjo_engine_.ReleaseImage(driver_image_id.ToBanjo());
}

zx::result<> EngineDriverClientBanjo::ReleaseCapture(
    display::DriverCaptureImageId driver_capture_image_id) {
  zx_status_t banjo_status = banjo_engine_.ReleaseCapture(driver_capture_image_id.ToBanjo());
  return zx::make_result(banjo_status);
}

display::ConfigCheckResult EngineDriverClientBanjo::CheckConfiguration(
    const display_config_t* display_config) {
  ZX_DEBUG_ASSERT(display_config != nullptr);
  ZX_DEBUG_ASSERT(display_config->layers_count != 0);
  ZX_DEBUG_ASSERT(display_config->layers_list != nullptr);

  config_check_result_t banjo_config_check_result =
      banjo_engine_.CheckConfiguration(display_config);
  if (!display::ConfigCheckResult::IsValid(banjo_config_check_result)) {
    fdf::error("Engine driver returned invalid ConfigCheck() result: {}",
               banjo_config_check_result);
    return display::ConfigCheckResult::kInvalidConfig;
  }
  display::ConfigCheckResult config_check_result(banjo_config_check_result);
  return config_check_result;
}

void EngineDriverClientBanjo::ApplyConfiguration(const display_config_t* display_config,
                                                 display::DriverConfigStamp config_stamp) {
  ZX_DEBUG_ASSERT(display_config != nullptr);
  ZX_DEBUG_ASSERT(display_config->layers_count != 0);
  ZX_DEBUG_ASSERT(display_config->layers_list != nullptr);

  const config_stamp_t banjo_config_stamp = config_stamp.ToBanjo();
  banjo_engine_.ApplyConfiguration(display_config, &banjo_config_stamp);
}

display::EngineInfo EngineDriverClientBanjo::CompleteCoordinatorConnection(
    const display_engine_listener_protocol_t& banjo_listener_protocol,
    fdf::ClientEnd<fuchsia_hardware_display_engine::EngineListener> fidl_listener_client) {
  engine_info_t banjo_engine_info;
  banjo_engine_.CompleteCoordinatorConnection(banjo_listener_protocol.ctx,
                                              banjo_listener_protocol.ops, &banjo_engine_info);
  if (!display::EngineInfo::IsValid(banjo_engine_info)) {
    fdf::fatal("CompleteCoordinatorConnection returned invalid EngineInfo");
  }
  return display::EngineInfo::From(banjo_engine_info);
}

void EngineDriverClientBanjo::UnsetListener() { banjo_engine_.UnsetListener(); }

zx::result<display::DriverImageId> EngineDriverClientBanjo::ImportImage(
    const display::ImageMetadata& image_metadata, display::DriverBufferCollectionId collection_id,
    uint32_t index) {
  const image_metadata_t banjo_image_metadata = image_metadata.ToBanjo();
  uint64_t image_handle = 0;
  zx_status_t banjo_status = banjo_engine_.ImportImage(
      &banjo_image_metadata, collection_id.ToBanjo(), index, &image_handle);
  if (banjo_status != ZX_OK) {
    return zx::error(banjo_status);
  }
  return zx::ok(display::DriverImageId(image_handle));
}

zx::result<display::DriverCaptureImageId> EngineDriverClientBanjo::ImportImageForCapture(
    display::DriverBufferCollectionId collection_id, uint32_t index) {
  uint64_t banjo_capture_image_handle = 0;
  zx_status_t banjo_status = banjo_engine_.ImportImageForCapture(collection_id.ToBanjo(), index,
                                                                 &banjo_capture_image_handle);
  if (banjo_status != ZX_OK) {
    return zx::error(banjo_status);
  }
  return zx::ok(display::DriverCaptureImageId(banjo_capture_image_handle));
}

zx::result<> EngineDriverClientBanjo::ImportBufferCollection(
    display::DriverBufferCollectionId collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> collection_token) {
  zx_status_t banjo_status = banjo_engine_.ImportBufferCollection(
      collection_id.ToBanjo(), std::move(collection_token).TakeChannel());
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClientBanjo::ReleaseBufferCollection(
    display::DriverBufferCollectionId collection_id) {
  zx_status_t banjo_status = banjo_engine_.ReleaseBufferCollection(collection_id.ToBanjo());
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClientBanjo::SetBufferCollectionConstraints(
    const display::ImageBufferUsage& usage, display::DriverBufferCollectionId collection_id) {
  const image_buffer_usage_t banjo_usage = usage.ToBanjo();
  zx_status_t banjo_status =
      banjo_engine_.SetBufferCollectionConstraints(&banjo_usage, collection_id.ToBanjo());
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClientBanjo::StartCapture(
    display::DriverCaptureImageId driver_capture_image_id) {
  zx_status_t banjo_status = banjo_engine_.StartCapture(driver_capture_image_id.ToBanjo());
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClientBanjo::SetDisplayPower(display::DisplayId display_id,
                                                      bool power_on) {
  zx_status_t banjo_status = banjo_engine_.SetDisplayPower(display_id.ToBanjo(), power_on);
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClientBanjo::SetMinimumRgb(uint8_t minimum_rgb) {
  zx_status_t banjo_status = banjo_engine_.SetMinimumRgb(minimum_rgb);
  return zx::make_result(banjo_status);
}

}  // namespace display_coordinator
