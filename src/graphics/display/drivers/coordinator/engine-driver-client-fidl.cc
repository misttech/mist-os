// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/engine-driver-client-fidl.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/arena.h>
#include <lib/fdf/dispatcher.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include "src/graphics/display/drivers/coordinator/banjo-fidl-conversion.h"

namespace display_coordinator {

namespace {
constexpr fdf_arena_tag_t kArenaTag = 'DISP';
}  // namespace

EngineDriverClientFidl::EngineDriverClientFidl(
    fdf::ClientEnd<fuchsia_hardware_display_engine::Engine> fidl_engine)
    : fidl_engine_(std::move(fidl_engine)) {
  ZX_DEBUG_ASSERT(fidl_engine_.is_valid());
}

EngineDriverClientFidl::~EngineDriverClientFidl() = default;

void EngineDriverClientFidl::ReleaseImage(display::DriverImageId driver_image_id) {
  fdf::Arena arena(kArenaTag);
  fidl::OneWayStatus fidl_transport_status =
      fidl_engine_.buffer(arena)->ReleaseImage(driver_image_id.ToFidl());
  if (!fidl_transport_status.ok()) {
    fdf::error("ReleaseImage failed: {}", fidl_transport_status.status_string());
  }
}

zx::result<> EngineDriverClientFidl::ReleaseCapture(
    display::DriverCaptureImageId driver_capture_image_id) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::ReleaseCapture>
      fidl_transport_result =
          fidl_engine_.buffer(arena)->ReleaseCapture(driver_capture_image_id.ToFidl());
  if (!fidl_transport_result.ok()) {
    fdf::error("ReleaseCapture failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok();
}

display::ConfigCheckResult EngineDriverClientFidl::CheckConfiguration(
    const display_config_t* display_config) {
  fdf::Arena arena(kArenaTag);
  fuchsia_hardware_display_engine::wire::DisplayConfig fidl_config =
      ToFidlDisplayConfig(*display_config, arena);

  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::CheckConfiguration>
      fidl_transport_result = fidl_engine_.buffer(arena)->CheckConfiguration(fidl_config);
  if (!fidl_transport_result.ok()) {
    fdf::error("CheckConfiguration failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }

  fit::result<fuchsia_hardware_display_types::ConfigResult> fidl_domain_result =
      fidl_transport_result.value();
  if (fidl_domain_result.is_error()) {
    return display::ConfigCheckResult(fidl_domain_result.error_value());
  }
  return display::ConfigCheckResult::kOk;
}

void EngineDriverClientFidl::ApplyConfiguration(const display_config_t* display_config,
                                                display::DriverConfigStamp config_stamp) {
  fdf::Arena arena(kArenaTag);
  fuchsia_hardware_display_engine::wire::DisplayConfig fidl_config =
      ToFidlDisplayConfig(*display_config, arena);

  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::ApplyConfiguration>
      fidl_transport_result =
          fidl_engine_.buffer(arena)->ApplyConfiguration(fidl_config, config_stamp.ToFidl());
  if (!fidl_transport_result.ok()) {
    fdf::error("ApplyConfiguration failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
}

display::EngineInfo EngineDriverClientFidl::CompleteCoordinatorConnection(
    const display_engine_listener_protocol_t& banjo_listener_protocol,
    fdf::ClientEnd<fuchsia_hardware_display_engine::EngineListener> fidl_listener_client) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::CompleteCoordinatorConnection>
      fidl_transport_result = fidl_engine_.buffer(arena)->CompleteCoordinatorConnection(
          std::move(fidl_listener_client));
  if (!fidl_transport_result.ok()) {
    fdf::error("CompleteCoordinatorConnection failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  return display::EngineInfo::From(fidl_transport_result->engine_info);
}

void EngineDriverClientFidl::UnsetListener() {
  fdf::Arena arena(kArenaTag);
  fidl::OneWayStatus fidl_transport_status = fidl_engine_.buffer(arena)->UnsetListener();
  if (!fidl_transport_status.ok()) {
    fdf::warn("UnsetListener failed: {}", fidl_transport_status.status_string());
  }
}

zx::result<display::DriverImageId> EngineDriverClientFidl::ImportImage(
    const display::ImageMetadata& image_metadata, display::DriverBufferCollectionId collection_id,
    uint32_t index) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::ImportImage>
      fidl_transport_result = fidl_engine_.buffer(arena)->ImportImage(
          image_metadata.ToFidl(), collection_id.ToFidl(), index);
  if (!fidl_transport_result.ok()) {
    fdf::error("ImportImage failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok(display::DriverImageId(fidl_transport_result->value()->image_id.value));
}

zx::result<display::DriverCaptureImageId> EngineDriverClientFidl::ImportImageForCapture(
    display::DriverBufferCollectionId collection_id, uint32_t index) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::ImportImageForCapture>
      fidl_transport_result =
          fidl_engine_.buffer(arena)->ImportImageForCapture(collection_id.ToFidl(), index);
  if (!fidl_transport_result.ok()) {
    fdf::error("ImportImageForCapture failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  fuchsia_hardware_display_engine::wire::ImageId image_id =
      fidl_transport_result->value()->capture_image_id;
  return zx::ok(display::DriverCaptureImageId(image_id.value));
}

zx::result<> EngineDriverClientFidl::ImportBufferCollection(
    display::DriverBufferCollectionId collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> collection_token) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::ImportBufferCollection>
      fidl_transport_result = fidl_engine_.buffer(arena)->ImportBufferCollection(
          collection_id.ToFidl(), std::move(collection_token));
  if (!fidl_transport_result.ok()) {
    fdf::error("ImportBufferCollection failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::ReleaseBufferCollection(
    display::DriverBufferCollectionId collection_id) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::ReleaseBufferCollection>
      fidl_transport_result =
          fidl_engine_.buffer(arena)->ReleaseBufferCollection(collection_id.ToFidl());
  if (!fidl_transport_result.ok()) {
    fdf::error("ReleaseBufferCollection failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::SetBufferCollectionConstraints(
    const display::ImageBufferUsage& usage, display::DriverBufferCollectionId collection_id) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::SetBufferCollectionConstraints>
      fidl_transport_result = fidl_engine_.buffer(arena)->SetBufferCollectionConstraints(
          usage.ToFidl(), collection_id.ToFidl());
  if (!fidl_transport_result.ok()) {
    fdf::error("SetBufferCollectionConstraints failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::StartCapture(
    display::DriverCaptureImageId driver_capture_image_id) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::StartCapture>
      fidl_transport_result =
          fidl_engine_.buffer(arena)->StartCapture(driver_capture_image_id.ToFidl());
  if (!fidl_transport_result.ok()) {
    fdf::error("StartCapture failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::SetDisplayPower(display::DisplayId display_id, bool power_on) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::SetDisplayPower>
      fidl_transport_result =
          fidl_engine_.buffer(arena)->SetDisplayPower(display_id.ToFidl(), power_on);
  if (!fidl_transport_result.ok()) {
    fdf::error("SetDisplayPower failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::SetMinimumRgb(uint8_t minimum_rgb) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult<fuchsia_hardware_display_engine::Engine::SetMinimumRgb>
      fidl_transport_result = fidl_engine_.buffer(arena)->SetMinimumRgb(minimum_rgb);
  if (!fidl_transport_result.ok()) {
    fdf::error("SetMinimumRgb failed: {}", fidl_transport_result.status_string());
    ZX_ASSERT(fidl_transport_result.ok());
  }
  if (fidl_transport_result->is_error()) {
    return zx::error(fidl_transport_result->error_value());
  }
  return zx::ok();
}

}  // namespace display_coordinator
