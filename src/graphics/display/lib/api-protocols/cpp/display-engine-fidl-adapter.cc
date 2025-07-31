// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-fidl-adapter.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <lib/fdf/dispatcher.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>

#include <sdk/lib/driver/logging/cpp/logger.h>

#include "src/graphics/display/lib/api-protocols/cpp/inplace-vector.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/engine-info.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace display {

DisplayEngineFidlAdapter::DisplayEngineFidlAdapter(DisplayEngineInterface* engine,
                                                   DisplayEngineEventsFidl* engine_events)
    : engine_(*engine), engine_events_(*engine_events) {
  ZX_DEBUG_ASSERT(engine != nullptr);
  ZX_DEBUG_ASSERT(engine_events != nullptr);
}

DisplayEngineFidlAdapter::~DisplayEngineFidlAdapter() = default;

fidl::ProtocolHandler<fuchsia_hardware_display_engine::Engine>
DisplayEngineFidlAdapter::CreateHandler(fdf_dispatcher_t& dispatcher) {
  return bindings_.CreateHandler(this, &dispatcher, fidl::kIgnoreBindingClosure);
}

void DisplayEngineFidlAdapter::CompleteCoordinatorConnection(
    fuchsia_hardware_display_engine::wire::EngineCompleteCoordinatorConnectionRequest* request,
    fdf::Arena& arena, CompleteCoordinatorConnectionCompleter::Sync& completer) {
  engine_events_.SetListener(std::move(request->engine_listener));
  display::EngineInfo engine_info = engine_.CompleteCoordinatorConnection();
  completer.buffer(arena).Reply(engine_info.ToFidl());
}

void DisplayEngineFidlAdapter::UnsetListener(fdf::Arena& arena,
                                             UnsetListenerCompleter::Sync& completer) {
  engine_events_.SetListener({});
}

void DisplayEngineFidlAdapter::ImportBufferCollection(
    fuchsia_hardware_display_engine::wire::EngineImportBufferCollectionRequest* request,
    fdf::Arena& arena, ImportBufferCollectionCompleter::Sync& completer) {
  const display::DriverBufferCollectionId driver_buffer_collection_id(
      request->buffer_collection_id);

  zx::result<> result = engine_.ImportBufferCollection(driver_buffer_collection_id,
                                                       std::move(request->collection_token));
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::ReleaseBufferCollection(
    fuchsia_hardware_display_engine::wire::EngineReleaseBufferCollectionRequest* request,
    fdf::Arena& arena, ReleaseBufferCollectionCompleter::Sync& completer) {
  const display::DriverBufferCollectionId driver_buffer_collection_id(
      request->buffer_collection_id);

  zx::result<> result = engine_.ReleaseBufferCollection(driver_buffer_collection_id);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::ImportImage(
    fuchsia_hardware_display_engine::wire::EngineImportImageRequest* request, fdf::Arena& arena,
    ImportImageCompleter::Sync& completer) {
  const display::ImageMetadata image_metadata = display::ImageMetadata(request->image_metadata);
  const display::DriverBufferCollectionId driver_buffer_collection_id(
      request->buffer_collection_id);

  zx::result<display::DriverImageId> result = engine_.ImportImage(
      image_metadata, driver_buffer_collection_id, request->buffer_collection_index);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess(result.value().ToFidl());
}

void DisplayEngineFidlAdapter::ImportImageForCapture(
    fuchsia_hardware_display_engine::wire::EngineImportImageForCaptureRequest* request,
    fdf::Arena& arena, ImportImageForCaptureCompleter::Sync& completer) {
  zx::result<display::DriverCaptureImageId> result = engine_.ImportImageForCapture(
      display::DriverBufferCollectionId(request->buffer_collection_id),
      request->buffer_collection_index);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess(result.value().ToFidl());
}

void DisplayEngineFidlAdapter::ReleaseImage(
    fuchsia_hardware_display_engine::wire::EngineReleaseImageRequest* request, fdf::Arena& arena,
    ReleaseImageCompleter::Sync& completer) {
  engine_.ReleaseImage(display::DriverImageId(request->image_id));
}

void DisplayEngineFidlAdapter::CheckConfiguration(
    fuchsia_hardware_display_engine::wire::EngineCheckConfigurationRequest* request,
    fdf::Arena& arena, CheckConfigurationCompleter::Sync& completer) {
  fuchsia_hardware_display_engine::wire::DisplayConfig& display_config = request->display_config;

  // The display coordinator currently uses zero-display configs to blank a
  // display. We'll remove this eventually.
  if (display_config.layers.empty()) {
    completer.buffer(arena).ReplySuccess();
    return;
  }

  if (display_config.layers.size() > display::EngineInfo::kMaxAllowedMaxLayerCount) {
    completer.buffer(arena).ReplyError(display::ConfigCheckResult::kUnsupportedConfig.ToFidl());
    return;
  }

  // This adapter does not currently support color correction.
  if (display_config.cc_flags != 0) {
    completer.buffer(arena).ReplyError(display::ConfigCheckResult::kUnsupportedConfig.ToFidl());
    return;
  }

  internal::InplaceVector<display::DriverLayer, display::EngineInfo::kMaxAllowedMaxLayerCount>
      layers;
  for (const auto& fidl_layer : display_config.layers) {
    ZX_DEBUG_ASSERT(display::DriverLayer::IsValid(fidl_layer));
    layers.emplace_back(fidl_layer);
  }

  display::ConfigCheckResult config_check_result = engine_.CheckConfiguration(
      display::DisplayId(display_config.display_id), display::ModeId(1), layers);

  if (config_check_result != display::ConfigCheckResult::kOk) {
    completer.buffer(arena).ReplyError(config_check_result.ToFidl());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::ApplyConfiguration(
    fuchsia_hardware_display_engine::wire::EngineApplyConfigurationRequest* request,
    fdf::Arena& arena, ApplyConfigurationCompleter::Sync& completer) {
  fuchsia_hardware_display_engine::wire::DisplayConfig& display_config = request->display_config;

  // The display coordinator currently uses zero-display configs to blank a
  // display. We'll remove this eventually.
  if (display_config.layers.empty()) {
    completer.buffer(arena).Reply();
    return;
  }

  ZX_DEBUG_ASSERT_MSG(display_config.layers.size() <= display::EngineInfo::kMaxAllowedMaxLayerCount,
                      "Display coordinator applied rejected config with too many layers");

  // This adapter does not currently support color correction.
  ZX_DEBUG_ASSERT_MSG(display_config.cc_flags == 0,
                      "Display coordinator applied rejected color-correction config");

  internal::InplaceVector<display::DriverLayer, display::EngineInfo::kMaxAllowedMaxLayerCount>
      layers;
  for (const auto& fidl_layer : display_config.layers) {
    ZX_DEBUG_ASSERT(display::DriverLayer::IsValid(fidl_layer));
    layers.emplace_back(fidl_layer);
  }

  engine_.ApplyConfiguration(display::DisplayId(display_config.display_id), display::ModeId(1),
                             layers, display::DriverConfigStamp(request->config_stamp));
  completer.buffer(arena).Reply();
}

void DisplayEngineFidlAdapter::SetBufferCollectionConstraints(
    fuchsia_hardware_display_engine::wire::EngineSetBufferCollectionConstraintsRequest* request,
    fdf::Arena& arena, SetBufferCollectionConstraintsCompleter::Sync& completer) {
  const display::ImageBufferUsage image_buffer_usage(request->usage);
  const display::DriverBufferCollectionId driver_buffer_collection_id(
      request->buffer_collection_id);

  zx::result<> result =
      engine_.SetBufferCollectionConstraints(image_buffer_usage, driver_buffer_collection_id);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::SetDisplayPower(
    fuchsia_hardware_display_engine::wire::EngineSetDisplayPowerRequest* request, fdf::Arena& arena,
    SetDisplayPowerCompleter::Sync& completer) {
  zx::result<> result =
      engine_.SetDisplayPower(display::DisplayId(request->display_id), request->power_on);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::SetMinimumRgb(
    fuchsia_hardware_display_engine::wire::EngineSetMinimumRgbRequest* request, fdf::Arena& arena,
    SetMinimumRgbCompleter::Sync& completer) {
  zx::result<> result = engine_.SetMinimumRgb(request->minimum_rgb);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::StartCapture(
    fuchsia_hardware_display_engine::wire::EngineStartCaptureRequest* request, fdf::Arena& arena,
    StartCaptureCompleter::Sync& completer) {
  const display::DriverCaptureImageId driver_capture_image_id(request->capture_image_id);

  zx::result<> result = engine_.StartCapture(driver_capture_image_id);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::ReleaseCapture(
    fuchsia_hardware_display_engine::wire::EngineReleaseCaptureRequest* request, fdf::Arena& arena,
    ReleaseCaptureCompleter::Sync& completer) {
  const display::DriverCaptureImageId driver_capture_image_id(request->capture_image_id);

  zx::result<> result = engine_.ReleaseCapture(driver_capture_image_id);
  if (result.is_error()) {
    completer.buffer(arena).ReplyError(result.error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void DisplayEngineFidlAdapter::IsAvailable(fdf::Arena& arena,
                                           IsAvailableCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void DisplayEngineFidlAdapter::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_display_engine::Engine> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(WARNING, "Dropping unknown FIDL method: %" PRIu64, metadata.method_ordinal);
}

}  // namespace display
