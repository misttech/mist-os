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
  fidl::OneWayStatus result = fidl_engine_.buffer(arena)->ReleaseImage(driver_image_id.ToFidl());
  if (!result.ok()) {
    fdf::error("ReleaseImage failed: {}", result.status_string());
  }
}

zx::result<> EngineDriverClientFidl::ReleaseCapture(
    display::DriverCaptureImageId driver_capture_image_id) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result =
      fidl_engine_.buffer(arena)->ReleaseCapture(driver_capture_image_id.ToFidl());
  if (!result.ok()) {
    fdf::error("ReleaseCapture failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

display::ConfigCheckResult EngineDriverClientFidl::CheckConfiguration(
    const display_config_t* display_config) {
  return display::ConfigCheckResult::kUnsupportedDisplayModes;
}

void EngineDriverClientFidl::ApplyConfiguration(const display_config_t* display_config,
                                                display::DriverConfigStamp config_stamp) {}

display::EngineInfo EngineDriverClientFidl::CompleteCoordinatorConnection(
    const display_engine_listener_protocol_t& protocol) {
  return display::EngineInfo({});
}

void EngineDriverClientFidl::UnsetListener() {}

zx::result<display::DriverImageId> EngineDriverClientFidl::ImportImage(
    const display::ImageMetadata& image_metadata, display::DriverBufferCollectionId collection_id,
    uint32_t index) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result = fidl_engine_.buffer(arena)->ImportImage(
      image_metadata.ToFidl(), collection_id.ToFidl(), index);
  if (!result.ok()) {
    fdf::error("ImportImage failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok(display::DriverImageId(result->value()->image_id.value));
}

zx::result<display::DriverCaptureImageId> EngineDriverClientFidl::ImportImageForCapture(
    display::DriverBufferCollectionId collection_id, uint32_t index) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result =
      fidl_engine_.buffer(arena)->ImportImageForCapture(collection_id.ToFidl(), index);
  if (!result.ok()) {
    fdf::error("ImportImageForCapture failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  fuchsia_hardware_display_engine::wire::ImageId image_id = result->value()->capture_image_id;
  return zx::ok(display::DriverCaptureImageId(image_id.value));
}

zx::result<> EngineDriverClientFidl::ImportBufferCollection(
    display::DriverBufferCollectionId collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> collection_token) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result = fidl_engine_.buffer(arena)->ImportBufferCollection(
      collection_id.ToFidl(), std::move(collection_token));
  if (!result.ok()) {
    fdf::error("ImportBufferCollection failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::ReleaseBufferCollection(
    display::DriverBufferCollectionId collection_id) {
  fdf::Arena arena(kArenaTag);
  auto result = fidl_engine_.buffer(arena)->ReleaseBufferCollection(collection_id.ToFidl());
  if (!result.ok()) {
    fdf::error("ReleaseBufferCollection failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::SetBufferCollectionConstraints(
    const display::ImageBufferUsage& usage, display::DriverBufferCollectionId collection_id) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result = fidl_engine_.buffer(arena)->SetBufferCollectionConstraints(
      usage.ToFidl(), collection_id.ToFidl());
  if (!result.ok()) {
    fdf::error("SetBufferCollectionConstraints failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::StartCapture(
    display::DriverCaptureImageId driver_capture_image_id) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result =
      fidl_engine_.buffer(arena)->StartCapture(driver_capture_image_id.ToFidl());
  if (!result.ok()) {
    fdf::error("StartCapture failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::SetDisplayPower(display::DisplayId display_id, bool power_on) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result =
      fidl_engine_.buffer(arena)->SetDisplayPower(display_id.ToFidl(), power_on);
  if (!result.ok()) {
    fdf::error("SetDisplayPower failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

zx::result<> EngineDriverClientFidl::SetMinimumRgb(uint8_t minimum_rgb) {
  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result = fidl_engine_.buffer(arena)->SetMinimumRgb(minimum_rgb);
  if (!result.ok()) {
    fdf::error("SetMinimumRgb failed: {}", result.status_string());
    ZX_ASSERT(result.ok());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

}  // namespace display_coordinator
