// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/client.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/defer.h>
#include <lib/image-format/image_format.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/sync/completion.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/trace/event.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <optional>
#include <ranges>
#include <span>
#include <string>
#include <utility>
#include <vector>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <fbl/string_printf.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/capture-image.h"
#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/client-proxy.h"
#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/lib/api-types/cpp/buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/buffer-id.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"

namespace fhd = fuchsia_hardware_display;
namespace fhdt = fuchsia_hardware_display_types;

namespace {

constexpr uint32_t kFallbackHorizontalSizeMm = 160;
constexpr uint32_t kFallbackVerticalSizeMm = 90;

// True iff `inner` is entirely contained within `outer`.
//
// `outer` must be positioned at the coordinate system's origin. Both `inner` and `outer` must be
// non-empty.
constexpr bool OriginRectangleContains(const rect_u_t& outer, const rect_u_t& inner) {
  ZX_DEBUG_ASSERT(outer.x == 0);
  ZX_DEBUG_ASSERT(outer.y == 0);
  ZX_DEBUG_ASSERT(outer.width > 0);
  ZX_DEBUG_ASSERT(outer.height > 0);
  ZX_DEBUG_ASSERT(inner.width > 0);
  ZX_DEBUG_ASSERT(inner.height > 0);

  return inner.x < outer.width && inner.y < outer.height && inner.x + inner.width <= outer.width &&
         inner.y + inner.height <= outer.height;
}

// We allocate some variable sized stack allocations based on the number of
// layers, so we limit the total number of layers to prevent blowing the stack.
constexpr uint64_t kMaxLayers = 65536;

}  // namespace

namespace display_coordinator {

void Client::ImportImage(ImportImageRequestView request, ImportImageCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::ImportImage");

  const display::ImageId image_id = display::ToImageId(request->image_id);
  if (image_id == display::kInvalidImageId) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto images_it = images_.find(image_id);
  if (images_it.IsValid()) {
    completer.ReplyError(ZX_ERR_ALREADY_EXISTS);
    return;
  }
  auto capture_image_it = capture_images_.find(image_id);
  if (capture_image_it.IsValid()) {
    completer.ReplyError(ZX_ERR_ALREADY_EXISTS);
    return;
  }

  if (!display::ImageMetadata::IsValid(request->image_metadata)) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  const display::ImageMetadata image_metadata(request->image_metadata);

  if (image_metadata.tiling_type() == display::ImageTilingType::kCapture) {
    zx_status_t import_status =
        ImportImageForCapture(image_metadata, display::ToBufferId(request->buffer_id), image_id);
    if (import_status == ZX_OK) {
      completer.ReplySuccess();
    } else {
      completer.ReplyError(import_status);
    }
    return;
  }

  zx_status_t import_status =
      ImportImageForDisplay(image_metadata, display::ToBufferId(request->buffer_id), image_id);
  if (import_status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(import_status);
  }
}

zx_status_t Client::ImportImageForDisplay(const display::ImageMetadata& image_metadata,
                                          display::BufferId buffer_id, display::ImageId image_id) {
  ZX_DEBUG_ASSERT(image_metadata.tiling_type() != display::ImageTilingType::kCapture);
  ZX_DEBUG_ASSERT(!images_.find(image_id).IsValid());
  ZX_DEBUG_ASSERT(!capture_images_.find(image_id).IsValid());

  auto collection_map_it = collection_map_.find(buffer_id.buffer_collection_id);
  if (collection_map_it == collection_map_.end()) {
    return ZX_ERR_INVALID_ARGS;
  }
  const Collections& collections = collection_map_it->second;

  zx::result<display::DriverImageId> result = controller_.engine_driver_client()->ImportImage(
      image_metadata, collections.driver_buffer_collection_id, buffer_id.buffer_index);
  if (result.is_error()) {
    return result.error_value();
  }

  const display::DriverImageId driver_image_id = result.value();
  auto release_image =
      fit::defer([this, driver_image_id]() { controller_.ReleaseImage(driver_image_id); });

  fbl::AllocChecker alloc_checker;
  fbl::RefPtr<Image> image = fbl::AdoptRef(new (&alloc_checker) Image(
      &controller_, image_metadata, image_id, driver_image_id, &proxy_->node(), id_));
  if (!alloc_checker.check()) {
    fdf::debug("Alloc checker failed while constructing Image.\n");
    return ZX_ERR_NO_MEMORY;
  }
  // `dc_image` is now owned by the Image instance.
  release_image.cancel();

  images_.insert(std::move(image));
  return ZX_OK;
}

void Client::ReleaseImage(ReleaseImageRequestView request,
                          ReleaseImageCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::ReleaseImage");

  const display::ImageId image_id = display::ToImageId(request->image_id);
  auto image = images_.find(image_id);
  if (image.IsValid()) {
    if (CleanUpImage(*image)) {
      ApplyConfigImpl();
    }
    return;
  }

  auto capture_image = capture_images_.find(image_id);
  if (capture_image.IsValid()) {
    // Ensure we are not releasing an active capture.
    if (current_capture_image_id_ == image_id) {
      // We have an active capture; release it when capture is completed.
      fdf::warn("Capture is active. Will release after capture is complete");
      pending_release_capture_image_id_ = current_capture_image_id_;
    } else {
      // Release image now.
      capture_images_.erase(capture_image);
    }
    return;
  }

  fdf::error("Invalid Image ID requested for release");
}

void Client::ImportEvent(ImportEventRequestView request,
                         ImportEventCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::ImportEvent");

  const display::EventId event_id = display::ToEventId(request->id);
  if (event_id == display::kInvalidEventId) {
    fdf::error("Cannot import events with an invalid ID #{}", event_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (zx_status_t status = fences_.ImportEvent(std::move(request->event), event_id);
      status != ZX_OK) {
    fdf::error("Failed to import event: {}", zx::make_result(status));
    TearDown(status);
    return;
  }
}

void Client::ImportBufferCollection(ImportBufferCollectionRequestView request,
                                    ImportBufferCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::ImportBufferCollection");

  const display::BufferCollectionId buffer_collection_id =
      display::ToBufferCollectionId(request->buffer_collection_id);
  // TODO: Switch to .contains() when C++20.
  if (collection_map_.count(buffer_collection_id)) {
    completer.ReplyError(ZX_ERR_ALREADY_EXISTS);
    return;
  }

  const display::DriverBufferCollectionId driver_buffer_collection_id =
      controller_.GetNextDriverBufferCollectionId();
  zx::result<> import_result = controller_.engine_driver_client()->ImportBufferCollection(
      driver_buffer_collection_id, std::move(request->buffer_collection_token));
  if (import_result.is_error()) {
    fdf::warn("Cannot import BufferCollection to display driver: {}", import_result);
    completer.ReplyError(ZX_ERR_INTERNAL);
    return;
  }

  collection_map_[buffer_collection_id] = Collections{
      .driver_buffer_collection_id = driver_buffer_collection_id,
  };
  completer.ReplySuccess();
}

void Client::ReleaseBufferCollection(ReleaseBufferCollectionRequestView request,
                                     ReleaseBufferCollectionCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::ReleaseBufferCollection");

  const display::BufferCollectionId buffer_collection_id =
      display::ToBufferCollectionId(request->buffer_collection_id);
  auto it = collection_map_.find(buffer_collection_id);
  if (it == collection_map_.end()) {
    return;
  }

  [[maybe_unused]] zx::result<> result =
      controller_.engine_driver_client()->ReleaseBufferCollection(
          it->second.driver_buffer_collection_id);
  if (result.is_error()) {
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
  }

  collection_map_.erase(it);
}

void Client::SetBufferCollectionConstraints(
    SetBufferCollectionConstraintsRequestView request,
    SetBufferCollectionConstraintsCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::SetBufferCollectionConstraints");

  const display::BufferCollectionId buffer_collection_id =
      display::ToBufferCollectionId(request->buffer_collection_id);
  auto it = collection_map_.find(buffer_collection_id);
  if (it == collection_map_.end()) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& collections = it->second;

  const display::ImageBufferUsage image_buffer_usage =
      display::ToImageBufferUsage(request->buffer_usage);
  zx::result<> result = controller_.engine_driver_client()->SetBufferCollectionConstraints(
      image_buffer_usage, collections.driver_buffer_collection_id);
  if (result.is_error()) {
    fdf::warn(
        "Cannot set BufferCollection constraints using imported buffer collection (id={}) {}.",
        buffer_collection_id.value(), result);
    completer.ReplyError(ZX_ERR_INTERNAL);
  }
  completer.ReplySuccess();
}

void Client::ReleaseEvent(ReleaseEventRequestView request,
                          ReleaseEventCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::ReleaseEvent");

  const display::EventId event_id = display::ToEventId(request->id);
  // TODO(https://fxbug.dev/42080337): Check if the ID is valid (i.e. imported but not
  // yet released) before calling `ReleaseEvent()`.
  fences_.ReleaseEvent(event_id);
}

void Client::CreateLayer(CreateLayerCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::CreateLayer");

  // TODO(https://fxbug.dev/42079482): Layer IDs should be client-managed.

  if (layers_.size() == kMaxLayers) {
    completer.ReplyError(ZX_ERR_NO_RESOURCES);
    return;
  }

  fbl::AllocChecker alloc_checker;
  display::DriverLayerId driver_layer_id = next_driver_layer_id++;
  auto new_layer = fbl::make_unique_checked<Layer>(&alloc_checker, &controller_, driver_layer_id);
  if (!alloc_checker.check()) {
    --driver_layer_id;
    completer.ReplyError(ZX_ERR_NO_MEMORY);
    return;
  }

  layers_.insert(std::move(new_layer));

  // Driver-side layer IDs are currently exposed to coordinator clients.
  // https://fxbug.dev/42079482 tracks having client-managed IDs. When that
  // happens, Client instances will be responsible for translating between
  // driver-side and client-side IDs.
  display::LayerId layer_id(driver_layer_id.value());
  completer.ReplySuccess(ToFidlLayerId(layer_id));
}

void Client::DestroyLayer(DestroyLayerRequestView request,
                          DestroyLayerCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::DestroyLayer");

  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());

  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    fdf::error("Tried to destroy invalid layer {}", layer_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (layer->in_use()) {
    fdf::error("Destroyed layer {} which was in use", layer_id.value());
    TearDown(ZX_ERR_BAD_STATE);
    return;
  }

  layers_.erase(driver_layer_id);
}

void Client::SetDisplayMode(SetDisplayModeRequestView request,
                            SetDisplayModeCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetDisplayMode");

  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  auto display_configs_it = display_configs_.find(display_id);
  if (!display_configs_it.IsValid()) {
    fdf::warn("SetDisplayMode called with unknown display ID: {}", display_id.value());
    return;
  }
  DisplayConfig& display_config = *display_configs_it;

  fbl::AutoLock lock(controller_.mtx());
  zx::result<std::span<const display::DisplayTiming>> display_timings_result =
      controller_.GetDisplayTimings(display_id);
  if (display_timings_result.is_error()) {
    fdf::error("SetDisplayMode failed to get display timings for display ID {}: {}",
               display_id.value(), display_timings_result);
    TearDown(display_timings_result.status_value());
    return;
  }

  std::span<const display::DisplayTiming> display_timings =
      std::move(display_timings_result).value();
  auto display_timing_it = std::find_if(
      display_timings.begin(), display_timings.end(),
      [&mode = request->mode](const display::DisplayTiming& timing) {
        if (timing.horizontal_active_px != static_cast<int32_t>(mode.active_area.width)) {
          return false;
        }
        if (timing.vertical_active_lines != static_cast<int32_t>(mode.active_area.height)) {
          return false;
        }
        if (timing.vertical_field_refresh_rate_millihertz() !=
            static_cast<int32_t>(mode.refresh_rate_millihertz)) {
          return false;
        }
        return true;
      });

  if (display_timing_it == display_timings.end()) {
    fdf::error("SetDisplayMode failed to find timings compatible with mode: {}x{} @ {}.{:03} Hz",
               request->mode.active_area.width, request->mode.active_area.height,
               request->mode.refresh_rate_millihertz / 1000,
               request->mode.refresh_rate_millihertz % 1000);
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  fdf::info("SetDisplayMode found supported display timing for {}x{} @ {}.{:03} Hz",
            display_timing_it->horizontal_active_px, display_timing_it->vertical_active_lines,
            display_timing_it->vertical_field_refresh_rate_millihertz() / 1000,
            display_timing_it->vertical_field_refresh_rate_millihertz() % 1000);

  if (display_timings.size() == 1) {
    fdf::info("SetDisplayMode skipping modeset, because the display only supports one timing.");
    return;
  }

  display_config.draft_.mode = display::ToBanjoDisplayMode(*display_timing_it);
  display_config.has_draft_nonlayer_config_change_ = true;
  draft_display_config_was_validated_ = false;
}

void Client::SetDisplayColorConversion(SetDisplayColorConversionRequestView request,
                                       SetDisplayColorConversionCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetDisplayColorConversion");

  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  auto display_configs_it = display_configs_.find(display_id);
  if (!display_configs_it.IsValid()) {
    fdf::warn("SetDisplayColorConversion called with unknown display ID: {}", display_id.value());
    return;
  }
  DisplayConfig& display_config = *display_configs_it;

  display_config.draft_.cc_flags = 0;
  if (!std::isnan(request->preoffsets[0])) {
    display_config.draft_.cc_flags |= COLOR_CONVERSION_PREOFFSET;
    std::memcpy(display_config.draft_.cc_preoffsets, request->preoffsets.data(),
                sizeof(request->preoffsets.data_));
    static_assert(sizeof(request->preoffsets) == sizeof(display_config.draft_.cc_preoffsets));
  }

  if (!std::isnan(request->coefficients[0])) {
    display_config.draft_.cc_flags |= COLOR_CONVERSION_COEFFICIENTS;
    std::memcpy(display_config.draft_.cc_coefficients, request->coefficients.data(),
                sizeof(request->coefficients.data_));
    static_assert(sizeof(request->coefficients) == sizeof(display_config.draft_.cc_coefficients));
  }

  if (!std::isnan(request->postoffsets[0])) {
    display_config.draft_.cc_flags |= COLOR_CONVERSION_POSTOFFSET;
    std::memcpy(display_config.draft_.cc_postoffsets, request->postoffsets.data(),
                sizeof(request->postoffsets.data_));
    static_assert(sizeof(request->postoffsets) == sizeof(display_config.draft_.cc_postoffsets));
  }

  display_config.has_draft_nonlayer_config_change_ = true;
  draft_display_config_was_validated_ = false;

  // One-way call. No reply required.
}

void Client::SetDisplayLayers(SetDisplayLayersRequestView request,
                              SetDisplayLayersCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetDisplayLayers");

  if (request->layer_ids.empty()) {
    fdf::error("SetDisplayLayers called with an empty layer list");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  auto display_configs_it = display_configs_.find(display_id);
  if (!display_configs_it.IsValid()) {
    fdf::warn("SetDisplayLayers called with unknown display ID: {}", display_id.value());
    return;
  }
  DisplayConfig& display_config = *display_configs_it;

  display_config.draft_has_layer_list_change_ = true;
  display_config.draft_has_layer_list_change_property_.Set(true);

  display_config.draft_layers_.clear();
  for (fuchsia_hardware_display::wire::LayerId fidl_layer_id : request->layer_ids) {
    display::LayerId layer_id = display::ToLayerId(fidl_layer_id);

    // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs,
    // the driver-side ID will have to be looked up in a map.
    display::DriverLayerId driver_layer_id(layer_id.value());
    auto layer = layers_.find(driver_layer_id);
    if (!layer.IsValid()) {
      fdf::error("SetDisplayLayers called with unknown layer ID: {}", layer_id.value());
      TearDown(ZX_ERR_INVALID_ARGS);
      return;
    }

    if (!layer->AppendToConfigLayerList(display_config.draft_layers_)) {
      fdf::error("Tried to reuse an in-use layer");
      TearDown(ZX_ERR_BAD_STATE);
      return;
    }
  }
  display_config.draft_.layer_count = static_cast<int32_t>(request->layer_ids.count());
  draft_display_config_was_validated_ = false;

  // One-way call. No reply required.
}

void Client::SetLayerPrimaryConfig(SetLayerPrimaryConfigRequestView request,
                                   SetLayerPrimaryConfigCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetLayerPrimaryConfig");

  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layers_it = layers_.find(driver_layer_id);
  if (!layers_it.IsValid()) {
    fdf::error("SetLayerPrimaryConfig called with unknown layer ID: {}", layer_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  Layer& layer = *layers_it;

  layer.SetPrimaryConfig(request->image_metadata);

  // TODO(https://fxbug.dev/397427767): Check if the layer belongs to the draft
  // config first.
  draft_display_config_was_validated_ = false;

  // One-way call. No reply required.
}

void Client::SetLayerPrimaryPosition(SetLayerPrimaryPositionRequestView request,
                                     SetLayerPrimaryPositionCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetLayerPrimaryPosition");

  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layers_it = layers_.find(driver_layer_id);
  if (!layers_it.IsValid()) {
    fdf::error("SetLayerPrimaryPosition called with unknown layer ID: {}", layer_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  Layer& layer = *layers_it;

  if (request->image_source_transformation > fhdt::wire::CoordinateTransformation::kRotateCcw270) {
    fdf::error("Invalid transform %" PRIu8,
               static_cast<uint8_t>(request->image_source_transformation));
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  layer.SetPrimaryPosition(request->image_source_transformation, request->image_source,
                           request->display_destination);

  // TODO(https://fxbug.dev/397427767): Check if the layer belongs to the draft
  // config first.
  draft_display_config_was_validated_ = false;

  // One-way call. No reply required.
}

void Client::SetLayerPrimaryAlpha(SetLayerPrimaryAlphaRequestView request,
                                  SetLayerPrimaryAlphaCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetLayerPrimaryAlpha");

  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layers_it = layers_.find(driver_layer_id);
  if (!layers_it.IsValid()) {
    fdf::error("SetLayerPrimaryAlpha called with unknown layer ID: {}", layer_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  Layer& layer = *layers_it;

  if (request->mode > fhdt::wire::AlphaMode::kHwMultiply ||
      (!isnan(request->val) && (request->val < 0 || request->val > 1))) {
    fdf::error("Invalid args {} {}", static_cast<uint8_t>(request->mode), request->val);
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  layer.SetPrimaryAlpha(request->mode, request->val);

  // TODO(https://fxbug.dev/397427767): Check if the layer belongs to the draft
  // config first.
  draft_display_config_was_validated_ = false;

  // One-way call. No reply required.
}

void Client::SetLayerColorConfig(SetLayerColorConfigRequestView request,
                                 SetLayerColorConfigCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetLayerColorConfig");

  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layers_it = layers_.find(driver_layer_id);
  if (!layers_it.IsValid()) {
    fdf::error("SetLayerColorConfig called with unknown layer ID: {}", layer_id.value());
    return;
  }
  Layer& layer = *layers_it;

  uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(PixelFormatAndModifier(
      request->color.format,
      /*pixel_format_modifier_param=*/fuchsia_images2::wire::PixelFormatModifier::kLinear));
  if (request->color.bytes.size() < bytes_per_pixel) {
    fdf::error("SetLayerColorConfig with invalid pixel format");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  layer.SetColorConfig(request->color);

  // TODO(https://fxbug.dev/397427767): Check if the layer belongs to the draft
  // config first.
  draft_display_config_was_validated_ = false;

  // One-way call. No reply required.
}

void Client::SetLayerImage2(SetLayerImage2RequestView request,
                            SetLayerImage2Completer::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetLayerImage2");

  SetLayerImageImpl(display::ToLayerId(request->layer_id), display::ToImageId(request->image_id),
                    display::ToEventId(request->wait_event_id));
}

void Client::SetLayerImageImpl(display::LayerId layer_id, display::ImageId image_id,
                               display::EventId wait_event_id) {
  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layers_it = layers_.find(driver_layer_id);
  if (!layers_it.IsValid()) {
    fdf::error("SetLayerImage called with unknown layer ID: {}", layer_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  Layer& layer = *layers_it;

  auto images_it = images_.find(image_id);
  if (!images_it.IsValid()) {
    fdf::error("SetLayerImage called with unknown image ID: {}", image_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  Image& image = *images_it;

  // TODO(https://fxbug.dev/42076907): Currently this logic only compares size
  // and usage type between the current `Image` and a given `Layer`'s accepted
  // configuration.
  //
  // We don't set the pixel format a `Layer` can accept, and we don't compare the
  // `Image` pixel format against any accepted pixel format, assuming that all
  // image buffers allocated by sysmem can always be used for scanout in any
  // `Layer`. Currently, this assumption works for all our existing display engine
  // drivers. However, switching pixel formats in a `Layer` may cause performance
  // reduction, or might be not supported by new display engines / new display
  // formats.
  //
  // We should figure out a mechanism to indicate pixel format / modifiers
  // support for a `Layer`'s image configuration (as opposed of using image_t),
  // and compare this Image's sysmem buffer collection information against the
  // `Layer`'s format support.
  if (image.metadata() != display::ImageMetadata(layer.draft_image_metadata())) {
    fdf::error("SetLayerImage with mismatching layer and image metadata");
    TearDown(ZX_ERR_BAD_STATE);
    return;
  }

  // TODO(https://fxbug.dev/42080337): Check if the IDs are valid (i.e. imported but not
  // yet released) before calling `SetImage()`.
  layer.SetImage(images_it.CopyPointer(), wait_event_id);

  // One-way call. No reply required.
}

void Client::CheckConfig(CheckConfigCompleter::Sync& completer) {
  display::ConfigCheckResult config_check_result = CheckConfigImpl();
  draft_display_config_was_validated_ = config_check_result == display::ConfigCheckResult::kOk;

  completer.Reply(config_check_result.ToFidl());
}

void Client::DiscardConfig(DiscardConfigCompleter::Sync& /*_completer*/) { DiscardConfig(); }

void Client::ApplyConfig3(ApplyConfig3RequestView request,
                          ApplyConfig3Completer::Sync& _completer) {
  TRACE_DURATION("gfx", "Display::Client::ApplyConfig3");

  if (!request->has_stamp()) {
    fdf::error("ApplyConfig3 called without a config stamp");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  const display::ConfigStamp new_config_stamp(request->stamp().value);

  if (layers_.is_empty()) {
    FDF_LOG(ERROR, "ApplyConfig3 called before SetDisplayLayers");
    TearDown(ZX_ERR_BAD_STATE);
    return;
  }

  if (!draft_display_config_was_validated_) {
    // TODO(https://fxbug.dev/397427767): TearDown(ZX_ERR_BAD_STATE) instead of
    // calling CheckConfig() and silently failing.
    draft_display_config_was_validated_ = CheckConfigImpl() == display::ConfigCheckResult::kOk;

    if (!draft_display_config_was_validated_) {
      fdf::info("ApplyConfig3 called with invalid configuration; dropping the request");
      return;
    }
  }

  // Now that we can guarantee that the configuration will be applied, it is
  // safe to update the config stamp.
  if (new_config_stamp <= latest_config_stamp_) {
    fdf::error(
        "ApplyConfig3 config stamp not monotonically increasing; new stamp: {}, previous stamp: {}",
        new_config_stamp.value(), latest_config_stamp_.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  latest_config_stamp_ = new_config_stamp;

  // Empty applied layer lists for all displays whose layer lists are changing.
  //
  // This guarantees that layers moved between displays don't end up in two
  // layer lists while each display's applied configuration is updated to match
  // its draft configuration.
  for (DisplayConfig& display_config : display_configs_) {
    if (display_config.draft_has_layer_list_change_) {
      display_config.applied_layers_.clear();
    }
  }

  for (DisplayConfig& display_config : display_configs_) {
    if (display_config.has_draft_nonlayer_config_change_) {
      display_config.applied_ = display_config.draft_;
      display_config.has_draft_nonlayer_config_change_ = false;
    }

    // Update any image layers. This needs to be done before migrating layers, as
    // that needs to know if there are any waiting images.
    for (LayerNode& draft_layer_node : display_config.draft_layers_) {
      if (!draft_layer_node.layer->ResolveDraftLayerProperties()) {
        fdf::error("Failed to resolve draft layer properties for layer {}",
                   draft_layer_node.layer->id().value());
        TearDown(ZX_ERR_BAD_STATE);
        return;
      }
      if (!draft_layer_node.layer->ResolveDraftImage(&fences_, latest_config_stamp_)) {
        fdf::error("Failed to resolve draft image for layer {}",
                   draft_layer_node.layer->id().value());
        TearDown(ZX_ERR_BAD_STATE);
        return;
      }
    }

    // Build applied layer lists that were emptied above.
    if (display_config.draft_has_layer_list_change_) {
      // Rebuild the applied layer list from the draft layer list.
      for (LayerNode& draft_layer_node : display_config.draft_layers_) {
        Layer* draft_layer = draft_layer_node.layer;
        display_config.applied_layers_.push_back(&draft_layer->applied_display_config_list_node_);
      }

      for (LayerNode& applied_layer_node : display_config.applied_layers_) {
        Layer* applied_layer = applied_layer_node.layer;
        // Don't migrate images between displays if there are pending images. See
        // `Controller::ApplyConfig` for more details.
        if (applied_layer->applied_to_display_id_ != display_config.id() &&
            applied_layer->applied_image_ != nullptr && applied_layer->HasWaitingImages()) {
          applied_layer->applied_image_ = nullptr;

          // This doesn't need to be reset anywhere, since we really care about the last
          // display this layer was shown on. Ignoring the 'null' display could cause
          // unusual layer changes to trigger this unnecessary, but that's not wrong.
          applied_layer->applied_to_display_id_ = display_config.id();
        }
      }
      display_config.draft_has_layer_list_change_ = false;
      display_config.draft_has_layer_list_change_property_.Set(false);
      display_config.pending_apply_layer_change_ = true;
      display_config.pending_apply_layer_change_property_.Set(true);
    }

    // Apply any draft configuration changes to active layers.
    for (LayerNode& applied_layer_node : display_config.applied_layers_) {
      applied_layer_node.layer->ApplyChanges();
    }
  }

  ApplyConfigImpl();

  // No reply defined.
}

void Client::GetLatestAppliedConfigStamp(GetLatestAppliedConfigStampCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::GetLatestAppliedConfigStamp");
  completer.Reply(ToFidlConfigStamp(latest_config_stamp_));
}

void Client::SetVsyncEventDelivery(SetVsyncEventDeliveryRequestView request,
                                   SetVsyncEventDeliveryCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetVsyncEventDelivery");
  proxy_->SetVsyncEventDelivery(request->vsync_delivery_enabled);
  // No reply defined.
}

void Client::SetVirtconMode(SetVirtconModeRequestView request,
                            SetVirtconModeCompleter::Sync& /*_completer*/) {
  TRACE_DURATION("gfx", "Display::Client::SetVirtconMode");

  if (priority_ != ClientPriority::kVirtcon) {
    fdf::error("SetVirtconMode() called by {} client", DebugStringFromClientPriority(priority_));
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  controller_.SetVirtconMode(request->mode);
  // No reply defined.
}

void Client::IsCaptureSupported(IsCaptureSupportedCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::IsCaptureSupported");
  completer.ReplySuccess(controller_.supports_capture());
}

zx_status_t Client::ImportImageForCapture(const display::ImageMetadata& image_metadata,
                                          display::BufferId buffer_id, display::ImageId image_id) {
  ZX_DEBUG_ASSERT(image_metadata.tiling_type() == display::ImageTilingType::kCapture);
  ZX_DEBUG_ASSERT(!images_.find(image_id).IsValid());
  ZX_DEBUG_ASSERT(!capture_images_.find(image_id).IsValid());

  // Ensure display driver supports/implements capture.
  if (!controller_.supports_capture()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Ensure a previously imported collection id is being used for import.
  auto it = collection_map_.find(buffer_id.buffer_collection_id);
  if (it == collection_map_.end()) {
    return ZX_ERR_INVALID_ARGS;
  }
  const Client::Collections& collections = it->second;
  zx::result<display::DriverCaptureImageId> import_result =
      controller_.engine_driver_client()->ImportImageForCapture(
          collections.driver_buffer_collection_id, buffer_id.buffer_index);
  if (import_result.is_error()) {
    return import_result.error_value();
  }
  const display::DriverCaptureImageId driver_capture_image_id = import_result.value();
  auto release_image = fit::defer([this, driver_capture_image_id]() {
    // TODO(https://fxbug.dev/42180237): Consider handling the error instead of ignoring it.
    [[maybe_unused]] zx::result<> result =
        controller_.engine_driver_client()->ReleaseCapture(driver_capture_image_id);
  });

  fbl::AllocChecker alloc_checker;
  fbl::RefPtr<CaptureImage> capture_image = fbl::AdoptRef(new (&alloc_checker) CaptureImage(
      &controller_, image_id, driver_capture_image_id, &proxy_->node(), id_));
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  // `driver_capture_image_id` is now owned by the CaptureImage instance.
  release_image.cancel();

  capture_images_.insert(std::move(capture_image));
  return ZX_OK;
}

void Client::StartCapture(StartCaptureRequestView request, StartCaptureCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::StartCapture");

  // Ensure display driver supports/implements capture.
  if (!controller_.supports_capture()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  // Don't start capture if one is in progress.
  if (current_capture_image_id_ != display::kInvalidImageId) {
    completer.ReplyError(ZX_ERR_SHOULD_WAIT);
    return;
  }

  // Ensure we have a capture fence for the request signal event.
  auto signal_fence = fences_.GetFence(display::ToEventId(request->signal_event_id));
  if (signal_fence == nullptr) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  // Ensure we are capturing into a valid image buffer.
  const display::ImageId capture_image_id = display::ToImageId(request->image_id);
  auto image = capture_images_.find(capture_image_id);
  if (!image.IsValid()) {
    fdf::error("Invalid Capture Image ID requested for capture");
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  capture_fence_id_ = display::ToEventId(request->signal_event_id);
  zx::result<> result =
      controller_.engine_driver_client()->StartCapture(image->driver_capture_image_id());
  if (result.is_error()) {
    completer.ReplyError(result.error_value());
    return;
  }

  fbl::AutoLock lock(controller_.mtx());
  proxy_->EnableCapture(true);
  completer.ReplySuccess();

  // Keep track of currently active capture image.
  current_capture_image_id_ = capture_image_id;  // TODO: Is this right?
}

void Client::SetMinimumRgb(SetMinimumRgbRequestView request,
                           SetMinimumRgbCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::SetMinimumRgb");

  if (!is_owner_) {
    completer.ReplyError(ZX_ERR_NOT_CONNECTED);
    return;
  }
  zx::result<> result = controller_.engine_driver_client()->SetMinimumRgb(request->minimum_rgb);
  if (result.is_error()) {
    completer.ReplyError(result.error_value());
    return;
  }
  client_minimum_rgb_ = request->minimum_rgb;
  completer.ReplySuccess();
}

void Client::SetDisplayPower(SetDisplayPowerRequestView request,
                             SetDisplayPowerCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Display::Client::SetDisplayPower");

  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  auto display_configs_it = display_configs_.find(display_id);
  if (!display_configs_it.IsValid()) {
    fdf::warn("SetDisplayPower called with unknown display ID: {}", display_id.value());
    completer.ReplyError(ZX_ERR_NOT_FOUND);
  }

  zx::result<> result =
      controller_.engine_driver_client()->SetDisplayPower(display_id, request->power_on);
  if (result.is_error()) {
    completer.ReplyError(result.error_value());
    return;
  }
  completer.ReplySuccess();
}

display::ConfigCheckResult Client::CheckConfigImpl() {
  TRACE_DURATION("gfx", "Display::Client::CheckConfig");

  for (const DisplayConfig& display_config : display_configs_) {
    if (display_config.draft_layers_.is_empty()) {
      // `SetDisplayLayers()` prevents the client from directly specifying an
      // empty layer list for a display. However, this can still happen if the
      // client put together a display configuration, a new display was added to
      // the system, and the client called CheckConfig() or ApplyConfig() before
      // it received the display change event.
      //
      // Skipping over the newly added display is appropriate, because display
      // engine drivers must support operating the hardware between the moment a
      // display is added and the moment it receives its first configuration.
      continue;
    }

    return CheckConfigForDisplay(display_config);
  }

  // The client needs to process display changes and prepare a configuration
  // that accounts for the added / removed displays.
  return display::ConfigCheckResult::kEmptyConfig;
}

display::ConfigCheckResult Client::CheckConfigForDisplay(const DisplayConfig& display_config) {
  ZX_DEBUG_ASSERT(!display_config.draft_layers_.is_empty());

  // The total number of registered layers is an upper bound on the number of
  // layers assigned to display configurations.
  const size_t max_layer_count = std::max(size_t{1}, layers_.size());

  // VLA is guaranteed  be non-empty (causing UB) thanks to the clamping above.
  //
  // TODO(https://fxbug.dev/42080896): Do not use VLA. We should introduce a limit on
  // totally supported layers instead.
  layer_t banjo_layers[max_layer_count];
  size_t banjo_layers_index = 0;

  // Frame used for checking that each layer's `display_destination` lies
  // entirely within the display output.
  const rect_u_t display_area = {
      .x = 0,
      .y = 0,
      .width = display_config.draft_.mode.h_addressable,
      .height = display_config.draft_.mode.v_addressable,
  };

  // Normalize the display configuration, and perform Coordinator-level
  // checks. The engine drivers API contract does not allow passing
  // configurations that fail these checks.
  for (const LayerNode& draft_layer_node : display_config.draft_layers_) {
    layer_t& banjo_layer = banjo_layers[banjo_layers_index];
    ++banjo_layers_index;

    banjo_layer = draft_layer_node.layer->draft_layer_config_;

    if (banjo_layer.image_source.width != 0 && banjo_layer.image_source.height != 0) {
      // Frame for checking that the layer's `image_source` lies entirely within
      // the source image.
      const rect_u_t image_area = {
          .x = 0,
          .y = 0,
          .width = banjo_layer.image_metadata.dimensions.width,
          .height = banjo_layer.image_metadata.dimensions.height,
      };
      if (!OriginRectangleContains(image_area, banjo_layer.image_source)) {
        return display::ConfigCheckResult::kInvalidConfig;
      }

      // The formats of layer images are negotiated by sysmem between clients
      // and display engine drivers when being imported, so they are always
      // accepted by the display coordinator.
    } else {
      // For now, solid color fill layers take up the entire area.
      // `SetColorLayer()` will be revised to explicitly configure an area for
      // the fill.
      banjo_layer.display_destination = display_area;

      // ApplyConfig() relies on CheckConfig() normalizing the layer. This
      // workaround can be removed when we revise `SetColorLayer()`.
      draft_layer_node.layer->draft_layer_config_.display_destination = display_area;
    }
    if (!OriginRectangleContains(display_area, banjo_layer.display_destination)) {
      return display::ConfigCheckResult::kInvalidConfig;
    }
  }

  ZX_DEBUG_ASSERT_MSG(display_config.draft_.layer_count == banjo_layers_index,
                      "Draft configuration layer count %zu does not agree with list size %zu",
                      display_config.draft_.layer_count, banjo_layers_index);

  // The layer count will be replaced if the client has a valid configuration
  // for a display.
  display_config_t banjo_display_config = display_config.draft_;
  banjo_display_config.layer_list = banjo_layers;

  {
    TRACE_DURATION("gfx", "Display::Client::CheckConfig engine_driver_client");
    return controller_.engine_driver_client()->CheckConfiguration(&banjo_display_config);
  }
}

void Client::ReapplyConfig() {
  if (latest_config_stamp_ != display::kInvalidConfigStamp) {
    ApplyConfigImpl();
  }
}

void Client::ApplyConfigImpl() {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher());
  TRACE_DURATION("gfx", "Display::Client::ApplyConfig internal");

  ZX_DEBUG_ASSERT_MSG(!layers_.is_empty(), "Empty layers during ApplyConfigImpl");

  bool config_missing_image = false;

  // The total number of registered layers is an upper bound on the number of
  // layers assigned to display configurations.
  //
  // This VLA (Variable-Length Array) is guaranteed not to be empty,
  // because SetDisplayLayers() requires a non-empty layer list.
  layer_t layers[layers_.size()];
  int layers_index = 0;

  // Layers may have pending images, and it is possible that a layer still
  // uses images from previous configurations. We should take this into account
  // when sending the config_stamp to `Controller`.
  //
  // We keep track of the "current client config stamp" for each image, the
  // value of which is only updated when a configuration uses an image that is
  // ready on application, or when the image's wait fence has been signaled and
  // `ActivateLatestReadyImage()` activates the new image.
  //
  // The final config_stamp sent to `Controller` will be the minimum of all
  // per-layer stamps.
  display::ConfigStamp applied_config_stamp = latest_config_stamp_;

  for (DisplayConfig& display_config : display_configs_) {
    display_config.applied_.layer_count = 0;
    display_config.applied_.layer_list = layers + layers_index;

    // Displays with no current layers are filtered out in `Controller::ApplyConfig`,
    // after it updates its own image tracking logic.

    for (LayerNode& applied_layer_node : display_config.applied_layers_) {
      Layer* applied_layer = applied_layer_node.layer;
      const bool activated = applied_layer->ActivateLatestReadyImage();
      if (activated && applied_layer->applied_image()) {
        display_config.pending_apply_layer_change_ = true;
        display_config.pending_apply_layer_change_property_.Set(true);
      }

      // This is subtle. Compute the config stamp for this config as the *earliest* stamp of any
      // `Image` that appears on a `Layer` in this config. The goal is to satisfy the contract of
      // the `applied_config_stamp` field of `CoordinatorListener.OnVsync()`, which returns the
      // config stamp of the latest *fully applied* config. For example, a config is not fully
      // applied if one of the images in the config is still waiting on a fence, even if the other
      // images in the config have appeared on-screen.
      std::optional<display::ConfigStamp> applied_layer_client_config_stamp =
          applied_layer->GetCurrentClientConfigStamp();
      if (applied_layer_client_config_stamp != std::nullopt) {
        applied_config_stamp = std::min(applied_config_stamp, *applied_layer_client_config_stamp);
      }

      display_config.applied_.layer_count++;
      layers[layers_index] = applied_layer->applied_layer_config_;
      ++layers_index;

      bool is_solid_color_fill = applied_layer->applied_layer_config_.image_source.width == 0 ||
                                 applied_layer->applied_layer_config_.image_source.height == 0;
      if (!is_solid_color_fill) {
        if (applied_layer->applied_image() == nullptr) {
          config_missing_image = true;
        }
      }
    }
  }

  if (!config_missing_image && is_owner_) {
    DisplayConfig* display_config_ptrs[std::max(size_t{1}, display_configs_.size())];
    size_t display_config_ptrs_index = 0;
    for (DisplayConfig& display_config : display_configs_) {
      display_config_ptrs[display_config_ptrs_index] = &display_config;
      ++display_config_ptrs_index;
    }

    controller_.ApplyConfig(
        std::span<DisplayConfig*>(display_config_ptrs, display_config_ptrs_index),
        applied_config_stamp, id_);
  }
}

void Client::SetOwnership(bool is_owner) {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher());
  is_owner_ = is_owner;

  fidl::Status result = NotifyOwnershipChange(/*client_has_ownership=*/is_owner);
  if (!result.ok()) {
    fdf::error("Error writing remove message: {}", result.FormatDescription());
  }

  // Only apply the current config if the client has previously applied a config.
  ReapplyConfig();
}

fidl::Status Client::NotifyDisplayChanges(
    std::span<const fuchsia_hardware_display::wire::Info> added_display_infos,
    std::span<const fuchsia_hardware_display_types::wire::DisplayId> removed_display_ids) {
  if (!coordinator_listener_.is_valid()) {
    return fidl::Status::Ok();
  }

  // TODO(https://fxbug.dev/42052765): `OnDisplayChanged()` takes `VectorView`s
  // of non-const display `Info` and `display::DisplayId` types though it doesn't modify
  // the vectors. We have to perform a `const_cast` to drop their constness.
  std::span<fuchsia_hardware_display::wire::Info> non_const_added_display_infos(
      const_cast<fuchsia_hardware_display::wire::Info*>(added_display_infos.data()),
      added_display_infos.size());
  std::span<fuchsia_hardware_display_types::wire::DisplayId> non_const_removed_display_ids(
      const_cast<fuchsia_hardware_display_types::wire::DisplayId*>(removed_display_ids.data()),
      removed_display_ids.size());

  fidl::OneWayStatus call_result = coordinator_listener_->OnDisplaysChanged(
      fidl::VectorView<fuchsia_hardware_display::wire::Info>::FromExternal(
          non_const_added_display_infos.data(), non_const_added_display_infos.size()),
      fidl::VectorView<fuchsia_hardware_display_types::wire::DisplayId>::FromExternal(
          non_const_removed_display_ids.data(), non_const_removed_display_ids.size()));
  return call_result;
}

fidl::Status Client::NotifyOwnershipChange(bool client_has_ownership) {
  if (!coordinator_listener_.is_valid()) {
    return fidl::Status::Ok();
  }

  fidl::OneWayStatus call_result =
      coordinator_listener_->OnClientOwnershipChange(client_has_ownership);
  return call_result;
}

fidl::Status Client::NotifyVsync(display::DisplayId display_id, zx::time timestamp,
                                 display::ConfigStamp config_stamp,
                                 display::VsyncAckCookie vsync_ack_cookie) {
  if (!coordinator_listener_.is_valid()) {
    return fidl::Status::Ok();
  }

  fidl::OneWayStatus send_call_result = coordinator_listener_->OnVsync(
      ToFidlDisplayId(display_id), timestamp.get(), ToFidlConfigStamp(config_stamp),
      ToFidlVsyncAckCookie(vsync_ack_cookie));
  return send_call_result;
}

void Client::OnDisplaysChanged(std::span<const display::DisplayId> added_display_ids,
                               std::span<const display::DisplayId> removed_display_ids) {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher());

  controller_.AssertMtxAliasHeld(*controller_.mtx());
  for (display::DisplayId added_display_id : added_display_ids) {
    fbl::AllocChecker alloc_checker;
    auto display_config = fbl::make_unique_checked<DisplayConfig>(&alloc_checker, added_display_id);
    if (!alloc_checker.check()) {
      fdf::warn("Out of memory when processing hotplug");
      continue;
    }

    zx::result get_supported_pixel_formats_result =
        controller_.GetSupportedPixelFormats(display_config->id());
    if (get_supported_pixel_formats_result.is_error()) {
      fdf::warn("Failed to get pixel formats when processing hotplug: {}",
                get_supported_pixel_formats_result);
      continue;
    }
    display_config->pixel_formats_ = std::move(get_supported_pixel_formats_result.value());

    zx::result<std::span<const display::DisplayTiming>> display_timings_result =
        controller_.GetDisplayTimings(display_config->id());
    if (display_timings_result.is_error()) {
      fdf::warn("Failed to get display timings when processing hotplug: {}",
                display_timings_result);
      continue;
    }

    display_config->applied_.display_id = ToBanjoDisplayId(display_config->id());
    display_config->applied_.layer_list = nullptr;
    display_config->applied_.layer_count = 0;

    std::span<const display::DisplayTiming> display_timings =
        std::move(display_timings_result).value();
    ZX_DEBUG_ASSERT(!display_timings.empty());
    display_config->applied_.mode = ToBanjoDisplayMode(display_timings[0]);

    display_config->applied_.cc_flags = 0;

    display_config->draft_ = display_config->applied_;

    display_config->InitializeInspect(&proxy_->node());

    display_configs_.insert(std::move(display_config));
  }

  // We need 2 loops, since we need to make sure we allocate the
  // correct size array in the FIDL response.
  std::vector<fhd::wire::Info> coded_configs;
  coded_configs.reserve(added_display_ids.size());

  // Hang on to modes values until we send the message.
  std::vector<std::vector<fuchsia_hardware_display_types::wire::Mode>> modes_vector;

  fidl::Arena arena;
  for (display::DisplayId added_display_id : added_display_ids) {
    auto display_configs_it = display_configs_.find(added_display_id);
    if (!display_configs_it.IsValid()) {
      // The display got removed before the display addition was processed and
      // reported to the client.
      continue;
    }
    const DisplayConfig& display_config = *display_configs_it;

    fhd::wire::Info fidl_display_info;
    fidl_display_info.id = display::ToFidlDisplayId(display_config.id());

    zx::result<std::span<const display::DisplayTiming>> display_timings_result =
        controller_.GetDisplayTimings(display_config.id());
    ZX_DEBUG_ASSERT(display_timings_result.is_ok());

    std::span<const display::DisplayTiming> display_timings = display_timings_result.value();
    ZX_DEBUG_ASSERT(!display_timings.empty());

    std::vector<fuchsia_hardware_display_types::wire::Mode> modes;

    modes.reserve(display_timings.size());
    for (const display::DisplayTiming& timing : display_timings) {
      modes.emplace_back(fuchsia_hardware_display_types::wire::Mode{
          .active_area =
              {
                  .width = static_cast<uint32_t>(timing.horizontal_active_px),
                  .height = static_cast<uint32_t>(timing.vertical_active_lines),
              },
          .refresh_rate_millihertz =
              static_cast<uint32_t>(timing.vertical_field_refresh_rate_millihertz()),
      });
    }
    modes_vector.emplace_back(std::move(modes));
    fidl_display_info.modes =
        fidl::VectorView<fuchsia_hardware_display_types::wire::Mode>::FromExternal(
            modes_vector.back());

    fidl_display_info.pixel_format = fidl::VectorView<fuchsia_images2::wire::PixelFormat>(
        arena, display_config.pixel_formats_.size());
    for (size_t pixel_format_index = 0; pixel_format_index < fidl_display_info.pixel_format.count();
         ++pixel_format_index) {
      fidl_display_info.pixel_format[pixel_format_index] =
          display_config.pixel_formats_[pixel_format_index].ToFidl();
    }

    const bool found_display_info =
        controller_.FindDisplayInfo(added_display_id, [&](const DisplayInfo& display_info) {
          fidl_display_info.manufacturer_name =
              fidl::StringView::FromExternal(display_info.GetManufacturerName());
          fidl_display_info.monitor_name = fidl::StringView(arena, display_info.GetMonitorName());
          fidl_display_info.monitor_serial =
              fidl::StringView(arena, display_info.GetMonitorSerial());

          // The return value of `GetHorizontalSizeMm()` is guaranteed to be `0 <= value < 2^16`,
          // so it can be safely cast to `uint32_t`.
          fidl_display_info.horizontal_size_mm =
              static_cast<uint32_t>(display_info.GetHorizontalSizeMm());

          // The return value of `GetVerticalSizeMm()` is guaranteed to be `0 <= value < 2^16`,
          // so it can be safely cast to uint32_t.
          fidl_display_info.vertical_size_mm =
              static_cast<uint32_t>(display_info.GetVerticalSizeMm());
        });
    if (!found_display_info) {
      fdf::error("Failed to get DisplayInfo for display {}", added_display_id.value());
      ZX_DEBUG_ASSERT(false);
    }

    fidl_display_info.using_fallback_size = false;
    if (fidl_display_info.horizontal_size_mm == 0 || fidl_display_info.vertical_size_mm == 0) {
      fidl_display_info.horizontal_size_mm = kFallbackHorizontalSizeMm;
      fidl_display_info.vertical_size_mm = kFallbackVerticalSizeMm;
      fidl_display_info.using_fallback_size = true;
    }

    coded_configs.push_back(fidl_display_info);
  }

  std::vector<fhdt::wire::DisplayId> fidl_removed_display_ids;
  fidl_removed_display_ids.reserve(removed_display_ids.size());

  for (display::DisplayId removed_display_id : removed_display_ids) {
    std::unique_ptr<DisplayConfig> display_config = display_configs_.erase(removed_display_id);
    if (display_config != nullptr) {
      display_config->draft_layers_.clear();
      display_config->applied_layers_.clear();
      fidl_removed_display_ids.push_back(display::ToFidlDisplayId(display_config->id()));
    }
  }

  if (!coded_configs.empty() || !fidl_removed_display_ids.empty()) {
    fidl::Status result = NotifyDisplayChanges(coded_configs, fidl_removed_display_ids);
    if (!result.ok()) {
      fdf::error("Error writing remove message: {}", result.FormatDescription());
    }
  }
}

void Client::OnFenceFired(FenceReference* fence) {
  bool new_image_ready = false;
  for (auto& layer : layers_) {
    new_image_ready |= layer.MarkFenceReady(fence);
  }
  if (new_image_ready) {
    ApplyConfigImpl();
  }
}

void Client::CaptureCompleted() {
  auto signal_fence = fences_.GetFence(capture_fence_id_);
  if (signal_fence != nullptr) {
    signal_fence->Signal();
  }

  // Release the pending capture image, if there is one.
  if (pending_release_capture_image_id_ != display::kInvalidImageId) {
    auto image = capture_images_.find(pending_release_capture_image_id_);
    if (image.IsValid()) {
      capture_images_.erase(image);
    }
    pending_release_capture_image_id_ = display::kInvalidImageId;
  }
  current_capture_image_id_ = display::kInvalidImageId;
}

void Client::TearDown(zx_status_t epitaph) {
  TRACE_DURATION("gfx", "Display::Client::TearDown");

  ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher());
  draft_display_config_was_validated_ = false;

  // See `fuchsia.hardware.display/Coordinator` protocol documentation in `coordinator.fidl`,
  // which describes the epitaph values that will be set when the channel closes.
  switch (epitaph) {
    case ZX_ERR_INVALID_ARGS:
    case ZX_ERR_BAD_STATE:
    case ZX_ERR_NO_MEMORY:
      fdf::info("TearDown() called with epitaph {}", zx::make_result(epitaph));
      break;
    default:
      fdf::info("TearDown() called with epitaph {}; using catchall ZX_ERR_INTERNAL instead",
                zx::make_result(epitaph));
      epitaph = ZX_ERR_INTERNAL;
  }

  // Teardown stops events from the channel, but not from the ddk, so we
  // need to make sure we don't try to teardown multiple times.
  if (!IsValid()) {
    return;
  }
  valid_ = false;

  // Break FIDL connections. First stop Vsync messages from the controller since there will be no
  // FIDL connections to forward the messages over.
  proxy_->SetVsyncEventDelivery(false);
  binding_->Close(epitaph);
  binding_.reset();
  coordinator_listener_.AsyncTeardown();

  CleanUpAllImages();
  fdf::info("Releasing {} capture images cur={}, pending={}", capture_images_.size(),
            current_capture_image_id_.value(), pending_release_capture_image_id_.value());
  current_capture_image_id_ = pending_release_capture_image_id_ = display::kInvalidImageId;
  capture_images_.clear();

  fences_.Clear();

  for (DisplayConfig& display_config : display_configs_) {
    display_config.draft_layers_.clear();
    display_config.applied_layers_.clear();
  }

  // The layer's images have already been handled in `CleanUpImageLayerState`.
  layers_.clear();

  // Release all imported buffer collections on display drivers.
  for (const auto& [k, v] : collection_map_) {
    // TODO(https://fxbug.dev/42180237): Consider handling the error instead of ignoring it.
    [[maybe_unused]] zx::result<> result =
        controller_.engine_driver_client()->ReleaseBufferCollection(v.driver_buffer_collection_id);
  }
  collection_map_.clear();
}

void Client::TearDownForTesting() { valid_ = false; }

bool Client::CleanUpAllImages() {
  // Clean up any layer state associated with the images.
  fbl::AutoLock lock(controller_.mtx());
  bool current_config_changed = std::any_of(layers_.begin(), layers_.end(), [this](Layer& layer) {
    controller_.AssertMtxAliasHeld(*layer.mtx());
    return layer.CleanUpAllImages();
  });

  images_.clear();
  return current_config_changed;
}

bool Client::CleanUpImage(Image& image) {
  // Clean up any layer state associated with the images.
  bool current_config_changed = false;

  {
    fbl::AutoLock lock(controller_.mtx());
    current_config_changed = std::any_of(layers_.begin(), layers_.end(),
                                         [this, &image = std::as_const(image)](Layer& layer) {
                                           controller_.AssertMtxAliasHeld(*layer.mtx());
                                           return layer.CleanUpImage(image);
                                         });
  }

  images_.erase(image);
  return current_config_changed;
}

void Client::CleanUpCaptureImage(display::ImageId id) {
  if (id == display::kInvalidImageId) {
    return;
  }
  // If the image is currently active, the underlying driver will retain a
  // handle to it until the hardware can be reprogrammed.
  auto image = capture_images_.find(id);
  if (image.IsValid()) {
    capture_images_.erase(image);
  }
}

void Client::SetAllConfigDraftLayersToAppliedLayers() {
  // Layers may have been moved between displays, so we must be extra careful
  // to avoid inserting a Layer in a display's draft list while it's
  // already moved to another Display's draft list.
  //
  // We side-step this problem by clearing all draft lists before inserting
  // any Layer in them, so that we can guarantee that for every Layer, its
  // `draft_node_` is not in any Display's draft list.
  for (DisplayConfig& display_config : display_configs_) {
    display_config.draft_layers_.clear();
  }
  for (DisplayConfig& display_config : display_configs_) {
    // Rebuild the draft layers list from applied layers list.
    for (LayerNode& layer_node : display_config.applied_layers_) {
      display_config.draft_layers_.push_back(&layer_node.layer->draft_display_config_list_node_);
    }
  }
}

void Client::DiscardConfig() {
  TRACE_DURATION("gfx", "Display::Client::DiscardConfig");

  // Go through layers and release any resources claimed by draft configs.
  for (Layer& layer : layers_) {
    layer.DiscardChanges();
  }

  // Discard layer list changes.
  SetAllConfigDraftLayersToAppliedLayers();

  // Discard the rest of the Display changes.
  for (DisplayConfig& display_config : display_configs_) {
    display_config.DiscardNonLayerDraftConfig();
  }
  draft_display_config_was_validated_ = true;
}

void Client::AcknowledgeVsync(AcknowledgeVsyncRequestView request,
                              AcknowledgeVsyncCompleter::Sync& /*_completer*/) {
  display::VsyncAckCookie ack_cookie = display::ToVsyncAckCookie(request->cookie);
  acked_cookie_ = ack_cookie;
  fdf::trace("Cookie {} Acked\n", ack_cookie.value());
}

void Client::Bind(
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server_end,
    fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener> coordinator_listener_client_end,
    fidl::OnUnboundFn<Client> unbound_callback) {
  ZX_DEBUG_ASSERT(!valid_);
  ZX_DEBUG_ASSERT(coordinator_server_end.is_valid());
  ZX_DEBUG_ASSERT(coordinator_listener_client_end.is_valid());
  valid_ = true;

  // Keep a copy of FIDL binding so we can safely unbind from it during shutdown.
  binding_ = fidl::BindServer(controller_.client_dispatcher()->async_dispatcher(),
                              std::move(coordinator_server_end), this, std::move(unbound_callback));

  coordinator_listener_.Bind(std::move(coordinator_listener_client_end),
                             controller_.client_dispatcher()->async_dispatcher());
}

Client::Client(Controller* controller, ClientProxy* proxy, ClientPriority priority,
               ClientId client_id)
    : controller_(*controller),
      proxy_(proxy),
      priority_(priority),
      id_(client_id),
      fences_(controller->client_dispatcher()->async_dispatcher(),
              fit::bind_member<&Client::OnFenceFired>(this)) {
  ZX_DEBUG_ASSERT(controller);
  ZX_DEBUG_ASSERT(proxy);
  ZX_DEBUG_ASSERT(client_id != kInvalidClientId);
}

Client::~Client() {
  ZX_DEBUG_ASSERT(!valid_);

  ZX_DEBUG_ASSERT(layers_.size() == 0);
}

}  // namespace display_coordinator
