// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/client.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
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
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <algorithm>
#include <atomic>
#include <cinttypes>
#include <cmath>
#include <cstdint>
#include <cstdlib>
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
#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
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
#include "src/graphics/display/lib/driver-utils/post-task.h"

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

// TODO(https://fxbug.dev/353627964): Make `AssertHeld()` a member function of `fbl::Mutex`.
void AssertHeld(fbl::Mutex& mutex) __TA_ASSERT(mutex) {
  ZX_DEBUG_ASSERT(mtx_trylock(mutex.GetInternal()) == thrd_busy);
}

}  // namespace

namespace display_coordinator {

void DisplayConfig::InitializeInspect(inspect::Node* parent) {
  static std::atomic_uint64_t inspect_count;
  node_ = parent->CreateChild(fbl::StringPrintf("display-config-%ld", inspect_count++).c_str());
  pending_layer_change_property_ = node_.CreateBool("pending_layer_change", pending_layer_change_);
  pending_apply_layer_change_property_ =
      node_.CreateBool("pending_apply_layer_change", pending_apply_layer_change_);
}

void DisplayConfig::DiscardNonLayerPendingConfig() {
  pending_layer_change_ = false;
  pending_layer_change_property_.Set(false);

  pending_ = current_;
  display_config_change_ = false;
}

void Client::ImportImage(ImportImageRequestView request, ImportImageCompleter::Sync& completer) {
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
      &controller_, image_metadata, driver_image_id, &proxy_->node(), id_));
  if (!alloc_checker.check()) {
    FDF_LOG(DEBUG, "Alloc checker failed while constructing Image.\n");
    return ZX_ERR_NO_MEMORY;
  }
  // `dc_image` is now owned by the Image instance.
  release_image.cancel();

  image->id = image_id;
  images_.insert(std::move(image));
  return ZX_OK;
}

void Client::ReleaseImage(ReleaseImageRequestView request,
                          ReleaseImageCompleter::Sync& /*_completer*/) {
  const display::ImageId image_id = display::ToImageId(request->image_id);
  auto image = images_.find(image_id);
  if (image.IsValid()) {
    if (CleanUpImage(*image)) {
      ApplyConfig();
    }
    return;
  }

  auto capture_image = capture_images_.find(image_id);
  if (capture_image.IsValid()) {
    // Ensure we are not releasing an active capture.
    if (current_capture_image_id_ == image_id) {
      // We have an active capture; release it when capture is completed.
      FDF_LOG(WARNING, "Capture is active. Will release after capture is complete");
      pending_release_capture_image_id_ = current_capture_image_id_;
    } else {
      // Release image now.
      capture_images_.erase(capture_image);
    }
    return;
  }

  FDF_LOG(ERROR, "Invalid Image ID requested for release");
}

void Client::ImportEvent(ImportEventRequestView request,
                         ImportEventCompleter::Sync& /*_completer*/) {
  const display::EventId event_id = display::ToEventId(request->id);
  if (event_id == display::kInvalidEventId) {
    FDF_LOG(ERROR, "Cannot import events with an invalid ID #%" PRIu64, event_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (zx_status_t status = fences_.ImportEvent(std::move(request->event), event_id);
      status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to import event: %s", zx_status_get_string(status));
    TearDown(status);
    return;
  }
}

void Client::ImportBufferCollection(ImportBufferCollectionRequestView request,
                                    ImportBufferCollectionCompleter::Sync& completer) {
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
    FDF_LOG(WARNING, "Cannot import BufferCollection to display driver: %s",
            import_result.status_string());
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
    FDF_LOG(WARNING,
            "Cannot set BufferCollection constraints using imported buffer collection (id=%lu) %s.",
            buffer_collection_id.value(), result.status_string());
    completer.ReplyError(ZX_ERR_INTERNAL);
  }
  completer.ReplySuccess();
}

void Client::ReleaseEvent(ReleaseEventRequestView request,
                          ReleaseEventCompleter::Sync& /*_completer*/) {
  const display::EventId event_id = display::ToEventId(request->id);
  // TODO(https://fxbug.dev/42080337): Check if the ID is valid (i.e. imported but not
  // yet released) before calling `ReleaseEvent()`.
  fences_.ReleaseEvent(event_id);
}

void Client::CreateLayer(CreateLayerCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/42079482): Layer IDs should be client-managed.

  if (layers_.size() == kMaxLayers) {
    completer.ReplyError(ZX_ERR_NO_RESOURCES);
    return;
  }

  fbl::AllocChecker alloc_checker;
  display::DriverLayerId driver_layer_id = next_driver_layer_id++;
  auto new_layer = fbl::make_unique_checked<Layer>(&alloc_checker, &controller_, driver_layer_id,
                                                   &layer_waiting_image_allocator_);
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
  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());

  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    FDF_LOG(ERROR, "Tried to destroy invalid layer %" PRIu64, layer_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (layer->in_use()) {
    FDF_LOG(ERROR, "Destroyed layer %" PRIu64 " which was in use", layer_id.value());
    TearDown(ZX_ERR_BAD_STATE);
    return;
  }

  layers_.erase(driver_layer_id);
}

void Client::SetDisplayMode(SetDisplayModeRequestView request,
                            SetDisplayModeCompleter::Sync& /*_completer*/) {
  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  auto config = configs_.find(display_id);
  if (!config.IsValid()) {
    return;
  }

  zx_status_t tear_down_status = ZX_OK;
  fit::deferred_action tear_down_on_error =
      fit::defer([this, &tear_down_status] { TearDown(tear_down_status); });

  fbl::AutoLock lock(controller_.mtx());
  zx::result<std::span<const display::DisplayTiming>> display_timings_result =
      controller_.GetDisplayTimings(display_id);
  if (display_timings_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get display timings for display #%" PRIu64 ": %s", display_id.value(),
            display_timings_result.status_string());
    tear_down_status = display_timings_result.status_value();
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
    FDF_LOG(ERROR, "Display mode not found: (%" PRIu32 " x %" PRIu32 ") @ %" PRIu32 " millihertz",
            request->mode.active_area.width, request->mode.active_area.height,
            request->mode.refresh_rate_millihertz);
    tear_down_status = ZX_ERR_NOT_FOUND;
    return;
  }

  if (display_timings.size() == 1) {
    FDF_LOG(INFO, "Display has only one timing available. Modeset is skipped.");
    tear_down_on_error.cancel();
    return;
  }

  config->pending_.mode = ToBanjoDisplayMode(*display_timing_it);
  pending_config_valid_ = false;
  config->display_config_change_ = true;
  tear_down_on_error.cancel();
}

void Client::SetDisplayColorConversion(SetDisplayColorConversionRequestView request,
                                       SetDisplayColorConversionCompleter::Sync& /*_completer*/) {
  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  auto config = configs_.find(display_id);
  if (!config.IsValid()) {
    return;
  }

  config->pending_.cc_flags = 0;
  if (!isnan(request->preoffsets[0])) {
    config->pending_.cc_flags |= COLOR_CONVERSION_PREOFFSET;
    memcpy(config->pending_.cc_preoffsets, request->preoffsets.data(),
           sizeof(request->preoffsets.data_));
    static_assert(sizeof(request->preoffsets) == sizeof(config->pending_.cc_preoffsets));
  }

  if (!isnan(request->coefficients[0])) {
    config->pending_.cc_flags |= COLOR_CONVERSION_COEFFICIENTS;
    memcpy(config->pending_.cc_coefficients, request->coefficients.data(),
           sizeof(request->coefficients.data_));
    static_assert(sizeof(request->coefficients) == sizeof(config->pending_.cc_coefficients));
  }

  if (!isnan(request->postoffsets[0])) {
    config->pending_.cc_flags |= COLOR_CONVERSION_POSTOFFSET;
    memcpy(config->pending_.cc_postoffsets, request->postoffsets.data(),
           sizeof(request->postoffsets.data_));
    static_assert(sizeof(request->postoffsets) == sizeof(config->pending_.cc_postoffsets));
  }

  config->display_config_change_ = true;
  pending_config_valid_ = false;
}

void Client::SetDisplayLayers(SetDisplayLayersRequestView request,
                              SetDisplayLayersCompleter::Sync& /*_completer*/) {
  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  auto config = configs_.find(display_id);
  if (!config.IsValid()) {
    FDF_LOG(WARNING, "SetDisplayLayers on display layer %lu", display_id.value());
    return;
  }

  config->pending_layer_change_ = true;
  config->pending_layer_change_property_.Set(true);
  config->pending_layers_.clear();
  for (uint64_t i = request->layer_ids.count() - 1; i != UINT64_MAX; i--) {
    display::LayerId layer_id = display::ToLayerId(request->layer_ids[i]);

    // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs,
    // the driver-side ID will have to be looked up in a map.
    display::DriverLayerId driver_layer_id(layer_id.value());
    auto layer = layers_.find(driver_layer_id);
    if (!layer.IsValid()) {
      FDF_LOG(ERROR, "SetDisplayLayers: unknown layer %lu", request->layer_ids[i].value);
      TearDown(ZX_ERR_INVALID_ARGS);
      return;
    }

    if (!layer->AppendToConfig(&config->pending_layers_)) {
      FDF_LOG(ERROR, "Tried to reuse an in-use layer");
      TearDown(ZX_ERR_BAD_STATE);
      return;
    }
  }
  config->pending_.layer_count = static_cast<int32_t>(request->layer_ids.count());
  pending_config_valid_ = false;
}

void Client::SetLayerPrimaryConfig(SetLayerPrimaryConfigRequestView request,
                                   SetLayerPrimaryConfigCompleter::Sync& /*_completer*/) {
  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    FDF_LOG(ERROR, "SetLayerPrimaryConfig on invalid layer");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  layer->SetPrimaryConfig(request->image_metadata);
  pending_config_valid_ = false;
  // No reply defined.
}

void Client::SetLayerPrimaryPosition(SetLayerPrimaryPositionRequestView request,
                                     SetLayerPrimaryPositionCompleter::Sync& /*_completer*/) {
  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    FDF_LOG(ERROR, "SetLayerPrimaryPosition on invalid layer");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (request->image_source_transformation > fhdt::wire::CoordinateTransformation::kRotateCcw270) {
    FDF_LOG(ERROR, "Invalid transform %" PRIu8,
            static_cast<uint8_t>(request->image_source_transformation));
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  layer->SetPrimaryPosition(request->image_source_transformation, request->image_source,
                            request->display_destination);
  pending_config_valid_ = false;
  // No reply defined.
}

void Client::SetLayerPrimaryAlpha(SetLayerPrimaryAlphaRequestView request,
                                  SetLayerPrimaryAlphaCompleter::Sync& /*_completer*/) {
  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    FDF_LOG(ERROR, "SetLayerPrimaryAlpha on invalid layer");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (request->mode > fhdt::wire::AlphaMode::kHwMultiply ||
      (!isnan(request->val) && (request->val < 0 || request->val > 1))) {
    FDF_LOG(ERROR, "Invalid args %hhu %f", static_cast<uint8_t>(request->mode), request->val);
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  layer->SetPrimaryAlpha(request->mode, request->val);
  pending_config_valid_ = false;
  // No reply defined.
}

void Client::SetLayerColorConfig(SetLayerColorConfigRequestView request,
                                 SetLayerColorConfigCompleter::Sync& /*_completer*/) {
  display::LayerId layer_id = display::ToLayerId(request->layer_id);

  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    FDF_LOG(ERROR, "SetLayerColorConfig on invalid layer");
    return;
  }

  uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(PixelFormatAndModifier(
      request->color.format,
      /*pixel_format_modifier_param=*/fuchsia_images2::wire::PixelFormatModifier::kLinear));
  if (request->color.bytes.size() < bytes_per_pixel) {
    FDF_LOG(ERROR, "SetLayerColorConfig with invalid pixel format");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  layer->SetColorConfig(request->color);
  pending_config_valid_ = false;
  // No reply defined.
}

void Client::SetLayerImage2(SetLayerImage2RequestView request,
                            SetLayerImage2Completer::Sync& /*_completer*/) {
  SetLayerImageImpl(display::ToLayerId(request->layer_id), display::ToImageId(request->image_id),
                    display::ToEventId(request->wait_event_id));
}

void Client::SetLayerImageImpl(display::LayerId layer_id, display::ImageId image_id,
                               display::EventId wait_event_id) {
  // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  display::DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    FDF_LOG(ERROR, "SetLayerImage with invalid layer ID: %" PRIu64, layer_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  auto image_it = images_.find(image_id);
  if (!image_it.IsValid()) {
    FDF_LOG(ERROR, "SetLayerImage with invalid image ID: %" PRIu64, image_id.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }

  Image& image = *image_it;

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
  if (image.metadata() != display::ImageMetadata(layer->pending_image_metadata())) {
    FDF_LOG(ERROR, "SetLayerImage with mismatching layer and image metadata");
    TearDown(ZX_ERR_BAD_STATE);
    return;
  }

  // TODO(https://fxbug.dev/42080337): Check if the IDs are valid (i.e. imported but not
  // yet released) before calling `SetImage()`.
  layer->SetImage(image_it.CopyPointer(), wait_event_id);
  // No reply defined.
}

void Client::CheckConfig(CheckConfigRequestView request, CheckConfigCompleter::Sync& completer) {
  fhdt::wire::ConfigResult res;
  std::vector<fhd::wire::ClientCompositionOp> ops;

  pending_config_valid_ = CheckConfig(&res, &ops);

  if (request->discard) {
    DiscardConfig();
  }

  completer.Reply(res, ::fidl::VectorView<fhd::wire::ClientCompositionOp>::FromExternal(ops));
}

void Client::ApplyConfig(ApplyConfigCompleter::Sync& /*_completer*/) {
  ApplyConfigFromFidl(latest_config_stamp_ + display::ConfigStamp(1));
}

void Client::ApplyConfig3(ApplyConfig3RequestView request, ApplyConfigCompleter::Sync& _completer) {
  if (!request->has_stamp()) {
    FDF_LOG(ERROR, "ApplyConfig3: stamp is required; none was provided");
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  ApplyConfigFromFidl(display::ConfigStamp(request->stamp().value));
}

void Client::ApplyConfigFromFidl(display::ConfigStamp new_config_stamp) {
  if (!pending_config_valid_) {
    pending_config_valid_ = CheckConfig(nullptr, nullptr);
    if (!pending_config_valid_) {
      FDF_LOG(INFO, "Client tried to apply invalid config");
      return;
    }
  }

  // Now that we can guarantee that the configuration will be applied, it is
  // safe to update the config stamp.
  if (new_config_stamp <= latest_config_stamp_) {
    FDF_LOG(ERROR,
            "Config stamp must be monotonically increasing. Previous stamp: %" PRIu64
            " New stamp: %" PRIu64,
            latest_config_stamp_.value(), new_config_stamp.value());
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  latest_config_stamp_ = new_config_stamp;

  // First go through and reset any current layer lists that are changing, so
  // we don't end up trying to put an image into two lists.
  for (auto& display_config : configs_) {
    if (display_config.pending_layer_change_) {
      // This guarantees that all nodes in `current_layers_` will be detached
      // so that a node can be put into another `current_layers_` list.
      display_config.current_layers_.clear();
    }
  }

  for (auto& display_config : configs_) {
    if (display_config.display_config_change_) {
      display_config.current_ = display_config.pending_;
      display_config.display_config_change_ = false;
    }

    // Update any image layers. This needs to be done before migrating layers, as
    // that needs to know if there are any waiting images.
    for (auto& layer_node : display_config.pending_layers_) {
      if (!layer_node.layer->ResolvePendingLayerProperties()) {
        FDF_LOG(ERROR, "Failed to resolve pending layer properties for layer %" PRIu64,
                layer_node.layer->id.value());
        TearDown(ZX_ERR_BAD_STATE);
        return;
      }
      if (!layer_node.layer->ResolvePendingImage(&fences_, latest_config_stamp_)) {
        FDF_LOG(ERROR, "Failed to resolve pending images for layer %" PRIu64,
                layer_node.layer->id.value());
        TearDown(ZX_ERR_BAD_STATE);
        return;
      }
    }

    // If there was a layer change, update the current layers list.
    if (display_config.pending_layer_change_) {
      for (LayerNode& layer_node : display_config.pending_layers_) {
        Layer* layer = layer_node.layer;
        // Rebuild current layer lists from pending layer lists.
        display_config.current_layers_.push_back(&layer->current_node_);
      }
      for (LayerNode& layer_node : display_config.current_layers_) {
        Layer* layer = layer_node.layer;
        // Don't migrate images between displays if there are pending images. See
        // `Controller::ApplyConfig` for more details.
        if (layer->current_display_id_ != display_config.id && layer->displayed_image_ &&
            layer->HasWaitingImages()) {
          layer->displayed_image_ = nullptr;

          // This doesn't need to be reset anywhere, since we really care about the last
          // display this layer was shown on. Ignoring the 'null' display could cause
          // unusual layer changes to trigger this unnecessary, but that's not wrong.
          layer->current_display_id_ = display_config.id;
        }
      }
      display_config.pending_layer_change_ = false;
      display_config.pending_layer_change_property_.Set(false);
      display_config.pending_apply_layer_change_ = true;
      display_config.pending_apply_layer_change_property_.Set(true);
    }

    // Apply any pending configuration changes to active layers.
    for (auto& layer_node : display_config.current_layers_) {
      layer_node.layer->ApplyChanges(display_config.current_.mode);
    }
  }
  // Overflow doesn't matter, since stamps only need to be unique until
  // the configuration is applied with vsync.
  client_apply_count_++;

  ApplyConfig();

  // No reply defined.
}

void Client::GetLatestAppliedConfigStamp(GetLatestAppliedConfigStampCompleter::Sync& completer) {
  completer.Reply(ToFidlConfigStamp(latest_config_stamp_));
}

void Client::SetVsyncEventDelivery(SetVsyncEventDeliveryRequestView request,
                                   SetVsyncEventDeliveryCompleter::Sync& /*_completer*/) {
  proxy_->SetVsyncEventDelivery(request->vsync_delivery_enabled);
  // No reply defined.
}

void Client::SetVirtconMode(SetVirtconModeRequestView request,
                            SetVirtconModeCompleter::Sync& /*_completer*/) {
  if (priority_ != ClientPriority::kVirtcon) {
    FDF_LOG(ERROR, "SetVirtconMode() called by %s client",
            DebugStringFromClientPriority(priority_));
    TearDown(ZX_ERR_INVALID_ARGS);
    return;
  }
  controller_.SetVirtconMode(request->mode);
  // No reply defined.
}

void Client::IsCaptureSupported(IsCaptureSupportedCompleter::Sync& completer) {
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
      &controller_, driver_capture_image_id, &proxy_->node(), id_));
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  // `driver_capture_image_id` is now owned by the CaptureImage instance.
  release_image.cancel();

  capture_image->id = image_id;
  capture_images_.insert(std::move(capture_image));
  return ZX_OK;
}

void Client::StartCapture(StartCaptureRequestView request, StartCaptureCompleter::Sync& completer) {
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
    FDF_LOG(ERROR, "Invalid Capture Image ID requested for capture");
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
  const display::DisplayId display_id = display::ToDisplayId(request->display_id);
  zx::result<> result =
      controller_.engine_driver_client()->SetDisplayPower(display_id, request->power_on);
  if (result.is_error()) {
    completer.ReplyError(result.error_value());
    return;
  }
  completer.ReplySuccess();
}

bool Client::CheckConfig(fhdt::wire::ConfigResult* res,
                         std::vector<fhd::wire::ClientCompositionOp>* ops) {
  if (res && ops) {
    *res = fhdt::wire::ConfigResult::kOk;
    ops->clear();
  }
  if (configs_.size() == 0) {
    // An empty config is always valid.
    return true;
  }
  const size_t layers_size = std::max(static_cast<size_t>(1), layers_.size());
  display_config_t configs[configs_.size()];
  layer_t layers[layers_size];

  // TODO(https://fxbug.dev/42080896): Do not use VLA. We should introduce a limit on
  // totally supported layers instead.
  layer_composition_operations_t layer_composition_operations[layers_size];
  memset(layer_composition_operations, 0, layers_size * sizeof(layer_composition_operations_t));
  int layer_composition_operations_count = 0;

  bool config_fail = false;
  size_t config_idx = 0;
  size_t layer_idx = 0;
  for (DisplayConfig& source_display_config : configs_) {
    if (source_display_config.pending_layers_.is_empty()) {
      continue;
    }

    // Put this display's `display_config_t` into the array.
    display_config_t& banjo_display_config = configs[config_idx];
    ++config_idx;

    // Create this display's compact `layer_t` array
    banjo_display_config = source_display_config.pending_;
    banjo_display_config.layer_list = layers + layer_idx;

    // Frame used for checking that each layer's `dest_frame` lies entirely
    // within the composed output.
    const rect_u_t display_area = {
        .x = 0,
        .y = 0,
        .width = banjo_display_config.mode.h_addressable,
        .height = banjo_display_config.mode.v_addressable,
    };

    // Do any work that needs to be done to make sure that the pending `layer_t` structs
    // are up to date, and validate that the configuration doesn't violate any API
    // constraints.
    for (LayerNode& source_layer_node : source_display_config.pending_layers_) {
      layer_t& banjo_layer = layers[layer_idx];
      ++layer_idx;

      banjo_layer = source_layer_node.layer->pending_layer_;
      ++layer_composition_operations_count;

      bool invalid = false;
      if (banjo_layer.image_source.width != 0 && banjo_layer.image_source.height != 0) {
        // Frame for checking that the layer's `image_source` lies entirely within
        // the source image.
        const rect_u_t image_area = {
            .x = 0,
            .y = 0,
            .width = banjo_layer.image_metadata.dimensions.width,
            .height = banjo_layer.image_metadata.dimensions.height,
        };
        invalid = !OriginRectangleContains(image_area, banjo_layer.image_source);

        // The formats of layer images are negotiated by sysmem between clients
        // and display engine drivers when being imported, so they are always
        // accepted by the display coordinator.
      } else {
        // For now, solid color fill layers take up the entire area.
        // `SetColorLayer()` will be revised to explicitly configure an area for
        // the fill.
        banjo_layer.display_destination = display_area;
      }
      invalid = invalid || !OriginRectangleContains(display_area, banjo_layer.display_destination);

      if (invalid) {
        // Continue to the next display, since there's nothing more to check for this one.
        config_fail = true;
        break;
      }
    }
  }

  if (config_fail) {
    if (res) {
      *res = fhdt::wire::ConfigResult::kInvalidConfig;
    }
    // If the config is invalid, there's no point in sending it to the impl driver.
    return false;
  }

  size_t layer_composition_operations_count_actual;
  config_check_result_t display_cfg_result = controller_.engine_driver_client()->CheckConfiguration(
      configs, config_idx, layer_composition_operations, layer_composition_operations_count,
      &layer_composition_operations_count_actual);

  switch (display_cfg_result) {
    case CONFIG_CHECK_RESULT_OK:
      if (res) {
        *res = fhdt::wire::ConfigResult::kOk;
      }
      return true;
    case CONFIG_CHECK_RESULT_INVALID_CONFIG:
      if (res) {
        *res = fhdt::wire::ConfigResult::kInvalidConfig;
      }
      return false;
    case CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG:
      if (res) {
        *res = fhdt::wire::ConfigResult::kUnsupportedConfig;
      }
      // Handle `ops` in the following steps.
      break;
    case CONFIG_CHECK_RESULT_TOO_MANY:
      if (res) {
        *res = fhdt::wire::ConfigResult::kTooManyDisplays;
      }
      return false;
    case CONFIG_CHECK_RESULT_UNSUPPORTED_MODES:
      if (res) {
        *res = fhdt::wire::ConfigResult::kUnsupportedDisplayModes;
      }
      return false;
  }

  ZX_DEBUG_ASSERT(display_cfg_result == CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG);

  std::span<const layer_composition_operations_t> layer_composition_operations_actual(
      layer_composition_operations, layer_composition_operations_count_actual);
  const bool layer_fail =
      std::ranges::any_of(layer_composition_operations_actual,
                          [](layer_composition_operations_t ops) { return ops != 0; });
  ZX_DEBUG_ASSERT_MSG(layer_fail,
                      "At least one layer must have non-empty LayerCompositionOperations");

  if (ops) {
    // TODO(b/249297195): Once Gerrit IFTTT supports multiple paths, add IFTTT
    // comments to make sure that any change of type `Client` in
    // //sdk/banjo/fuchsia.hardware.display.controller/display-controller.fidl
    // will cause the definition of `kAllErrors` to change as well.
    static constexpr layer_composition_operations_t kAllOperations =
        LAYER_COMPOSITION_OPERATIONS_USE_IMAGE | LAYER_COMPOSITION_OPERATIONS_MERGE_BASE |
        LAYER_COMPOSITION_OPERATIONS_MERGE_SRC | LAYER_COMPOSITION_OPERATIONS_FRAME_SCALE |
        LAYER_COMPOSITION_OPERATIONS_SRC_FRAME | LAYER_COMPOSITION_OPERATIONS_TRANSFORM |
        LAYER_COMPOSITION_OPERATIONS_COLOR_CONVERSION | LAYER_COMPOSITION_OPERATIONS_ALPHA;

    layer_idx = 0;
    for (auto& display_config : configs_) {
      if (display_config.pending_layers_.is_empty()) {
        continue;
      }

      bool seen_base = false;
      for (auto& layer_node : display_config.pending_layers_) {
        uint32_t composition_operations = kAllOperations & layer_composition_operations[layer_idx];
        // Fixup the error flags if the driver impl incorrectly set multiple MERGE_BASEs.
        if (composition_operations & LAYER_COMPOSITION_OPERATIONS_MERGE_BASE) {
          if (seen_base) {
            composition_operations &= ~LAYER_COMPOSITION_OPERATIONS_MERGE_BASE;
            composition_operations |= LAYER_COMPOSITION_OPERATIONS_MERGE_SRC;
          } else {
            seen_base = true;
            composition_operations &= ~LAYER_COMPOSITION_OPERATIONS_MERGE_SRC;
          }
        }

        // TODO(https://fxbug.dev/42079482): When switching to client-managed IDs,
        // the client-side ID will have to be looked up in a map.
        const display::LayerId layer_id(layer_node.layer->id.value());

        for (uint8_t i = 0; i < 32; i++) {
          if (composition_operations & (1 << i)) {
            ops->emplace_back(fhd::wire::ClientCompositionOp{
                .display_id = ToFidlDisplayId(display_config.id),
                .layer_id = ToFidlLayerId(layer_id),
                .opcode = static_cast<fhdt::wire::ClientCompositionOpcode>(i),
            });
          }
        }
        layer_idx++;
      }
    }
  }
  return false;
}

void Client::ApplyConfig() {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher());
  TRACE_DURATION("gfx", "Display::Client::ApplyConfig");

  bool config_missing_image = false;
  // Clients can apply zero-layer configs. Ensure that the VLA is at least 1 element long.
  layer_t layers[layers_.size() + 1];
  int layer_idx = 0;

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
  display::ConfigStamp current_applied_config_stamp = latest_config_stamp_;

  for (auto& display_config : configs_) {
    display_config.current_.layer_count = 0;
    display_config.current_.layer_list = layers + layer_idx;

    // Displays with no current layers are filtered out in `Controller::ApplyConfig`,
    // after it updates its own image tracking logic.

    for (auto& layer_node : display_config.current_layers_) {
      Layer* layer = layer_node.layer;
      const bool activated = layer->ActivateLatestReadyImage();
      if (activated && layer->current_image()) {
        display_config.pending_apply_layer_change_ = true;
        display_config.pending_apply_layer_change_property_.Set(true);
      }

      std::optional<display::ConfigStamp> layer_client_config_stamp =
          layer->GetCurrentClientConfigStamp();
      if (layer_client_config_stamp) {
        current_applied_config_stamp =
            std::min(current_applied_config_stamp, *layer_client_config_stamp);
      }

      display_config.current_.layer_count++;
      layers[layer_idx++] = layer->current_layer_;
      bool is_solid_color_fill = layer->current_layer_.image_source.width == 0 ||
                                 layer->current_layer_.image_source.height == 0;
      if (!is_solid_color_fill) {
        if (layer->current_image() == nullptr) {
          config_missing_image = true;
        }
      }
    }
  }

  if (!config_missing_image && is_owner_) {
    DisplayConfig* dc_configs[configs_.size() + 1];
    int dc_idx = 0;
    for (auto& c : configs_) {
      dc_configs[dc_idx++] = &c;
    }

    controller_.ApplyConfig(dc_configs, dc_idx, current_applied_config_stamp, client_apply_count_,
                            id_);
  }
}

void Client::SetOwnership(bool is_owner) {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher());
  is_owner_ = is_owner;

  fidl::Status result = NotifyOwnershipChange(/*client_has_ownership=*/is_owner);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Error writing remove message: %s", result.FormatDescription().c_str());
  }

  // Only apply the current config if the client has previously applied a config.
  if (client_apply_count_) {
    ApplyConfig();
  }
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
    fbl::AllocChecker ac;
    auto config = fbl::make_unique_checked<DisplayConfig>(&ac);
    if (!ac.check()) {
      FDF_LOG(WARNING, "Out of memory when processing hotplug");
      continue;
    }

    config->id = added_display_id;

    zx::result get_supported_pixel_formats_result =
        controller_.GetSupportedPixelFormats(config->id);
    if (get_supported_pixel_formats_result.is_error()) {
      FDF_LOG(WARNING, "Failed to get pixel formats when processing hotplug: %s",
              get_supported_pixel_formats_result.status_string());
      continue;
    }
    config->pixel_formats_ = std::move(get_supported_pixel_formats_result.value());

    zx::result<std::span<const display::DisplayTiming>> display_timings_result =
        controller_.GetDisplayTimings(config->id);
    if (display_timings_result.is_error()) {
      FDF_LOG(WARNING, "Failed to get display timings when processing hotplug: %s",
              display_timings_result.status_string());
      continue;
    }

    config->current_.display_id = ToBanjoDisplayId(config->id);
    config->current_.layer_list = nullptr;
    config->current_.layer_count = 0;

    std::span<const display::DisplayTiming> display_timings =
        std::move(display_timings_result).value();
    ZX_DEBUG_ASSERT(!display_timings.empty());
    config->current_.mode = ToBanjoDisplayMode(display_timings[0]);

    config->current_.cc_flags = 0;

    config->pending_ = config->current_;

    config->InitializeInspect(&proxy_->node());

    configs_.insert(std::move(config));
  }

  // We need 2 loops, since we need to make sure we allocate the
  // correct size array in the FIDL response.
  std::vector<fhd::wire::Info> coded_configs;
  coded_configs.reserve(added_display_ids.size());

  // Hang on to modes values until we send the message.
  std::vector<std::vector<fuchsia_hardware_display_types::wire::Mode>> modes_vector;

  fidl::Arena arena;
  for (display::DisplayId added_display_id : added_display_ids) {
    auto config = configs_.find(added_display_id);
    if (!config.IsValid()) {
      continue;
    }

    fhd::wire::Info info;
    info.id = ToFidlDisplayId(config->id);

    zx::result<std::span<const display::DisplayTiming>> display_timings_result =
        controller_.GetDisplayTimings(config->id);
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
    info.modes = fidl::VectorView<fuchsia_hardware_display_types::wire::Mode>::FromExternal(
        modes_vector.back());

    info.pixel_format =
        fidl::VectorView<fuchsia_images2::wire::PixelFormat>(arena, config->pixel_formats_.size());
    for (size_t pixel_format_index = 0; pixel_format_index < info.pixel_format.count();
         ++pixel_format_index) {
      info.pixel_format[pixel_format_index] = config->pixel_formats_[pixel_format_index].ToFidl();
    }

    const bool found_display_info =
        controller_.FindDisplayInfo(added_display_id, [&](const DisplayInfo& display_info) {
          info.manufacturer_name =
              fidl::StringView::FromExternal(display_info.GetManufacturerName());
          info.monitor_name = fidl::StringView(arena, display_info.GetMonitorName());
          info.monitor_serial = fidl::StringView(arena, display_info.GetMonitorSerial());

          // The return value of `GetHorizontalSizeMm()` is guaranteed to be `0 <= value < 2^16`,
          // so it can be safely cast to `uint32_t`.
          info.horizontal_size_mm = static_cast<uint32_t>(display_info.GetHorizontalSizeMm());

          // The return value of `GetVerticalSizeMm()` is guaranteed to be `0 <= value < 2^16`,
          // so it can be safely cast to uint32_t.
          info.vertical_size_mm = static_cast<uint32_t>(display_info.GetVerticalSizeMm());
        });
    if (!found_display_info) {
      FDF_LOG(ERROR, "Failed to get DisplayInfo for display %" PRIu64, added_display_id.value());
      ZX_DEBUG_ASSERT(false);
    }

    info.using_fallback_size = false;
    if (info.horizontal_size_mm == 0 || info.vertical_size_mm == 0) {
      info.horizontal_size_mm = kFallbackHorizontalSizeMm;
      info.vertical_size_mm = kFallbackVerticalSizeMm;
      info.using_fallback_size = true;
    }

    coded_configs.push_back(info);
  }

  std::vector<fhdt::wire::DisplayId> fidl_removed_display_ids;
  fidl_removed_display_ids.reserve(removed_display_ids.size());

  for (display::DisplayId removed_display_id : removed_display_ids) {
    auto display = configs_.erase(removed_display_id);
    if (display) {
      display->pending_layers_.clear();
      display->current_layers_.clear();
      fidl_removed_display_ids.push_back(ToFidlDisplayId(display->id));
    }
  }

  if (!coded_configs.empty() || !fidl_removed_display_ids.empty()) {
    fidl::Status result = NotifyDisplayChanges(coded_configs, fidl_removed_display_ids);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Error writing remove message: %s", result.FormatDescription().c_str());
    }
  }
}

void Client::OnFenceFired(FenceReference* fence) {
  bool new_image_ready = false;
  for (auto& layer : layers_) {
    new_image_ready |= layer.OnFenceReady(fence);
  }
  if (new_image_ready) {
    ApplyConfig();
  }
}

void Client::CaptureCompleted() {
  auto signal_fence = fences_.GetFence(capture_fence_id_);
  if (signal_fence != nullptr) {
    signal_fence->Signal();
  }

  // release any pending capture images
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
  ZX_DEBUG_ASSERT(controller_.IsRunningOnClientDispatcher());
  pending_config_valid_ = false;

  // See `fuchsia.hardware.display/Coordinator` protocol documentation in `coordinator.fidl`,
  // which describes the epitaph values that will be set when the channel closes.
  switch (epitaph) {
    case ZX_ERR_INVALID_ARGS:
    case ZX_ERR_BAD_STATE:
    case ZX_ERR_NO_MEMORY:
      FDF_LOG(INFO, "TearDown() called with epitaph %s", zx_status_get_string(epitaph));
      break;
    default:
      FDF_LOG(INFO, "TearDown() called with epitaph %s; using catchall ZX_ERR_INTERNAL instead",
              zx_status_get_string(epitaph));
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
  FDF_LOG(INFO, "Releasing %zu capture images cur=%" PRIu64 ", pending=%" PRIu64,
          capture_images_.size(), current_capture_image_id_.value(),
          pending_release_capture_image_id_.value());
  current_capture_image_id_ = pending_release_capture_image_id_ = display::kInvalidImageId;
  capture_images_.clear();

  fences_.Clear();

  for (auto& config : configs_) {
    config.pending_layers_.clear();
    config.current_layers_.clear();
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

  ApplyConfig();
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

void Client::SetAllConfigPendingLayersToCurrentLayers() {
  // Layers may have been moved between displays, so we must be extra careful
  // to avoid inserting a Layer in a display's pending list while it's
  // already moved to another Display's pending list.
  //
  // We side-step this problem by clearing all pending lists before inserting
  // any Layer in them, so that we can guarantee that for every Layer, its
  // `pending_node_` is not in any Display's pending list.
  for (auto& config : configs_) {
    config.pending_layers_.clear();
  }
  for (auto& config : configs_) {
    // Rebuild the pending layers list from current layers list.
    for (LayerNode& layer_node : config.current_layers_) {
      config.pending_layers_.push_back(&layer_node.layer->pending_node_);
    }
  }
}

void Client::DiscardConfig() {
  // Go through layers and release any pending resources they claimed.
  for (Layer& layer : layers_) {
    layer.DiscardChanges();
  }

  // Discard the changes to Display layers lists.
  //
  // Reset pending layers lists of all displays to their current layers, respectively.
  SetAllConfigPendingLayersToCurrentLayers();

  // Discard the rest of the Display changes.
  for (DisplayConfig& config : configs_) {
    config.DiscardNonLayerPendingConfig();
  }
  pending_config_valid_ = true;
}

void Client::AcknowledgeVsync(AcknowledgeVsyncRequestView request,
                              AcknowledgeVsyncCompleter::Sync& /*_completer*/) {
  display::VsyncAckCookie ack_cookie = display::ToVsyncAckCookie(request->cookie);
  acked_cookie_ = ack_cookie;
  FDF_LOG(TRACE, "Cookie %" PRIu64 " Acked\n", ack_cookie.value());
}

std::string GetObjectName(zx_handle_t handle) {
  char name[ZX_MAX_NAME_LEN];
  zx_status_t status = zx_object_get_property(handle, ZX_PROP_NAME, name, sizeof(name));
  return status == ZX_OK ? std::string(name) : std::string();
}

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
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
              fit::bind_member<&Client::OnFenceFired>(this)),
      // TODO(https://fxbug.dev/370839049): this assumes that clients won't exhaust a single slab's
      // worth of allocations. Scenic's usage patterns shouldn't trigger this. Anyway, a CL is
      // already in progress to replace this slab allocation approach, so this is a short-term
      // concern.
      layer_waiting_image_allocator_(/*max_slabs=*/1) {
  ZX_DEBUG_ASSERT(controller);
  ZX_DEBUG_ASSERT(proxy);
  ZX_DEBUG_ASSERT(client_id != kInvalidClientId);
}

Client::~Client() {
  ZX_DEBUG_ASSERT(!valid_);

  ZX_DEBUG_ASSERT(layers_.size() == 0);
#ifndef NDEBUG
  // obj_count() is only available in debug builds.
  ZX_ASSERT(layer_waiting_image_allocator_.obj_count() == 0);
#endif
}

void ClientProxy::SetOwnership(bool is_owner) {
  fbl::AllocChecker ac;
  auto task = fbl::make_unique_checked<async::Task>(&ac);
  if (!ac.check()) {
    FDF_LOG(WARNING, "Failed to allocate set ownership task");
    return;
  }
  task->set_handler([this, client_handler = &handler_, is_owner](
                        async_dispatcher_t* /*dispatcher*/, async::Task* task, zx_status_t status) {
    if (status == ZX_OK && client_handler->IsValid()) {
      is_owner_property_.Set(is_owner);
      client_handler->SetOwnership(is_owner);
    }
    // Update `client_scheduled_tasks_`.
    fbl::AutoLock task_lock(&task_mtx_);
    auto it = std::find_if(client_scheduled_tasks_.begin(), client_scheduled_tasks_.end(),
                           [&](std::unique_ptr<async::Task>& t) { return t.get() == task; });
    // Current task must have been added to the list.
    ZX_DEBUG_ASSERT(it != client_scheduled_tasks_.end());
    client_scheduled_tasks_.erase(it);
  });
  fbl::AutoLock task_lock(&task_mtx_);
  if (task->Post(controller_.client_dispatcher()->async_dispatcher()) == ZX_OK) {
    client_scheduled_tasks_.push_back(std::move(task));
  }
}

void ClientProxy::OnDisplaysChanged(std::span<const display::DisplayId> added_display_ids,
                                    std::span<const display::DisplayId> removed_display_ids) {
  handler_.OnDisplaysChanged(added_display_ids, removed_display_ids);
}

void ClientProxy::ReapplySpecialConfigs() {
  AssertHeld(*controller_.mtx());

  zx::result<> result = controller_.engine_driver_client()->SetMinimumRgb(handler_.GetMinimumRgb());
  if (!result.is_ok()) {
    FDF_LOG(ERROR, "Failed to reapply minimum RGB value: %s", result.status_string());
  }
}

void ClientProxy::ReapplyConfig() {
  fbl::AllocChecker ac;
  auto task = fbl::make_unique_checked<async::Task>(&ac);
  if (!ac.check()) {
    FDF_LOG(WARNING, "Failed to reapply config");
    return;
  }

  task->set_handler([this, client_handler = &handler_](async_dispatcher_t* /*dispatcher*/,
                                                       async::Task* task, zx_status_t status) {
    if (status == ZX_OK && client_handler->IsValid()) {
      client_handler->ApplyConfig();
    }
    // Update `client_scheduled_tasks_`.
    fbl::AutoLock task_lock(&task_mtx_);
    auto it = std::find_if(client_scheduled_tasks_.begin(), client_scheduled_tasks_.end(),
                           [&](std::unique_ptr<async::Task>& t) { return t.get() == task; });
    // Current task must have been added to the list.
    ZX_DEBUG_ASSERT(it != client_scheduled_tasks_.end());
    client_scheduled_tasks_.erase(it);
  });
  fbl::AutoLock task_lock(&task_mtx_);
  if (task->Post(controller_.client_dispatcher()->async_dispatcher()) == ZX_OK) {
    client_scheduled_tasks_.push_back(std::move(task));
  }
}

zx_status_t ClientProxy::OnCaptureComplete() {
  AssertHeld(*controller_.mtx());
  fbl::AutoLock l(&mtx_);
  if (enable_capture_) {
    handler_.CaptureCompleted();
  }
  enable_capture_ = false;
  return ZX_OK;
}

zx_status_t ClientProxy::OnDisplayVsync(display::DisplayId display_id, zx_time_t timestamp,
                                        display::ConfigStamp controller_stamp) {
  AssertHeld(*controller_.mtx());
  fidl::Status event_sending_result = fidl::Status::Ok();

  display::ConfigStamp client_stamp = {};
  auto it =
      std::find_if(pending_applied_config_stamps_.begin(), pending_applied_config_stamps_.end(),
                   [controller_stamp](const ConfigStampPair& stamp) {
                     return stamp.controller_stamp >= controller_stamp;
                   });

  if (it == pending_applied_config_stamps_.end() || it->controller_stamp != controller_stamp) {
    client_stamp = display::kInvalidConfigStamp;
  } else {
    client_stamp = it->client_stamp;
    pending_applied_config_stamps_.erase(pending_applied_config_stamps_.begin(), it);
  }

  {
    fbl::AutoLock l(&mtx_);
    if (!vsync_delivery_enabled_) {
      return ZX_ERR_NOT_SUPPORTED;
    }
  }

  display::VsyncAckCookie vsync_ack_cookie = display::kInvalidVsyncAckCookie;
  if (number_of_vsyncs_sent_ >= (kVsyncMessagesWatermark - 1)) {
    // Number of  vsync events sent exceed the watermark level.
    // Check to see if client has been notified already that acknowledgement is needed.
    if (!acknowledge_request_sent_) {
      // We have not sent a (new) cookie to client for acknowledgement; do it now.
      // First, increment cookie sequence.
      cookie_sequence_++;
      // Generate new cookie by xor'ing initial cookie with sequence number.
      vsync_ack_cookie = initial_cookie_ ^ cookie_sequence_;
    } else {
      // Client has already been notified; check if client has acknowledged it.
      ZX_DEBUG_ASSERT(last_cookie_sent_ != display::kInvalidVsyncAckCookie);
      if (handler_.LatestAckedCookie() == last_cookie_sent_) {
        // Client has acknowledged cookie. Reset vsync tracking states
        number_of_vsyncs_sent_ = 0;
        acknowledge_request_sent_ = false;
        last_cookie_sent_ = display::kInvalidVsyncAckCookie;
      }
    }
  }

  if (number_of_vsyncs_sent_ >= kMaxVsyncMessages) {
    // We have reached/exceeded maximum allowed vsyncs without any acknowledgement. At this point,
    // start storing them.
    FDF_LOG(TRACE, "Vsync not sent due to none acknowledgment.\n");
    ZX_DEBUG_ASSERT(vsync_ack_cookie == display::kInvalidVsyncAckCookie);
    if (buffered_vsync_messages_.full()) {
      buffered_vsync_messages_.pop();  // discard
    }
    buffered_vsync_messages_.push(VsyncMessageData{
        .display_id = display_id,
        .timestamp = timestamp,
        .config_stamp = client_stamp,
    });
    return ZX_ERR_BAD_STATE;
  }

  auto cleanup = fit::defer([&]() {
    if (vsync_ack_cookie != display::kInvalidVsyncAckCookie) {
      cookie_sequence_--;
    }
    // Make sure status is not `ZX_ERR_BAD_HANDLE`, otherwise channel write may crash (depending on
    // policy setting).
    ZX_DEBUG_ASSERT(event_sending_result.status() != ZX_ERR_BAD_HANDLE);
    if (event_sending_result.status() == ZX_ERR_NO_MEMORY) {
      total_oom_errors_++;
      // OOM errors are most likely not recoverable. Print the error message
      // once every kChannelErrorPrintFreq cycles.
      if (chn_oom_print_freq_++ == 0) {
        FDF_LOG(ERROR, "Failed to send vsync event (OOM) (total occurrences: %lu)",
                total_oom_errors_);
      }
      if (chn_oom_print_freq_ >= kChannelOomPrintFreq) {
        chn_oom_print_freq_ = 0;
      }
    } else {
      FDF_LOG(WARNING, "Failed to send vsync event: %s",
              event_sending_result.FormatDescription().c_str());
    }
  });

  // Send buffered vsync events before sending the latest.
  while (!buffered_vsync_messages_.empty()) {
    VsyncMessageData vsync_message_data = buffered_vsync_messages_.front();
    buffered_vsync_messages_.pop();
    event_sending_result =
        handler_.NotifyVsync(vsync_message_data.display_id, zx::time{vsync_message_data.timestamp},
                             vsync_message_data.config_stamp, display::kInvalidVsyncAckCookie);
    if (!event_sending_result.ok()) {
      FDF_LOG(ERROR, "Failed to send all buffered vsync messages: %s\n",
              event_sending_result.FormatDescription().c_str());
      return event_sending_result.status();
    }
    number_of_vsyncs_sent_++;
  }

  // Send the latest vsync event.
  event_sending_result =
      handler_.NotifyVsync(display_id, zx::time{timestamp}, client_stamp, vsync_ack_cookie);
  if (!event_sending_result.ok()) {
    return event_sending_result.status();
  }

  // Update vsync tracking states.
  if (vsync_ack_cookie != display::kInvalidVsyncAckCookie) {
    acknowledge_request_sent_ = true;
    last_cookie_sent_ = vsync_ack_cookie;
  }
  number_of_vsyncs_sent_++;
  cleanup.cancel();
  return ZX_OK;
}

void ClientProxy::OnClientDead() {
  // Stash any data members we need to access after the ClientProxy is deleted.
  fit::function<void()> on_client_disconnected = std::move(on_client_disconnected_);

  // Deletes `this`.
  controller_.OnClientDead(this);

  on_client_disconnected();
}

void ClientProxy::UpdateConfigStampMapping(ConfigStampPair stamps) {
  ZX_DEBUG_ASSERT(pending_applied_config_stamps_.empty() ||
                  pending_applied_config_stamps_.back().controller_stamp < stamps.controller_stamp);
  pending_applied_config_stamps_.push_back({
      .controller_stamp = stamps.controller_stamp,
      .client_stamp = stamps.client_stamp,
  });
}

void ClientProxy::CloseForTesting() { handler_.TearDownForTesting(); }

void ClientProxy::CloseOnControllerLoop() {
  // Tasks only fail to post if the looper is dead. That can happen if the
  // controller is unbinding and shutting down active clients, but if it does
  // then it's safe to call Reset on this thread anyway.
  [[maybe_unused]] zx::result<> post_task_result = display::PostTask<kDisplayTaskTargetSize>(
      *controller_.client_dispatcher()->async_dispatcher(),
      // `Client::TearDown()` must be called even if the task fails to post.
      [_ = display::CallFromDestructor(
           [this]() { handler_.TearDown(ZX_ERR_CONNECTION_ABORTED); })]() {});
}

zx_status_t ClientProxy::Init(
    inspect::Node* parent_node,
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server_end,
    fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
        coordinator_listener_client_end) {
  node_ =
      parent_node->CreateChild(fbl::StringPrintf("client-%" PRIu64, handler_.id().value()).c_str());
  node_.RecordString("priority", DebugStringFromClientPriority(handler_.priority()));
  is_owner_property_ = node_.CreateBool("is_owner", false);

  unsigned seed = static_cast<unsigned>(zx::clock::get_monotonic().get());
  initial_cookie_ = display::VsyncAckCookie(rand_r(&seed));

  fidl::OnUnboundFn<Client> unbound_callback =
      [this](Client* client, fidl::UnbindInfo info,
             fidl::ServerEnd<fuchsia_hardware_display::Coordinator> ch) {
        sync_completion_signal(client->fidl_unbound());
        // Make sure we `TearDown()` so that no further tasks are scheduled on the controller loop.
        client->TearDown(ZX_OK);

        // The client has died. Notify the proxy, which will free the classes.
        OnClientDead();
      };

  handler_.Bind(std::move(coordinator_server_end), std::move(coordinator_listener_client_end),
                std::move(unbound_callback));
  return ZX_OK;
}

zx::result<> ClientProxy::InitForTesting(
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server_end,
    fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
        coordinator_listener_client_end) {
  // `ClientProxy` created by tests may not have a full-fledged display engine.
  // The production client teardown logic doesn't work here so we replace it with a no-op unbound
  // callback instead.
  fidl::OnUnboundFn<Client> unbound_callback =
      [](Client*, fidl::UnbindInfo, fidl::ServerEnd<fuchsia_hardware_display::Coordinator>) {};
  handler_.Bind(std::move(coordinator_server_end), std::move(coordinator_listener_client_end),
                std::move(unbound_callback));
  return zx::ok();
}

ClientProxy::ClientProxy(Controller* controller, ClientPriority client_priority, ClientId client_id,
                         fit::function<void()> on_client_disconnected)
    : controller_(*controller),
      handler_(&controller_, this, client_priority, client_id),
      on_client_disconnected_(std::move(on_client_disconnected)) {
  ZX_DEBUG_ASSERT(controller);
  ZX_DEBUG_ASSERT(on_client_disconnected_);
}

ClientProxy::~ClientProxy() {}

}  // namespace display_coordinator

// Checks the FIDL `ClientCompositionOpcode` enum matches the corresponding bits
// in banjo `ClientCompositionOpcode` bitfield.
//
// TODO(https://fxbug.dev/42080698): In the short term, instead of checking this in
// Coordinator, a bridging type should be used for conversion of the types. In
// the long term, these two types should be unified.
namespace {

static_assert((1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientUseImage)) ==
                  LAYER_COMPOSITION_OPERATIONS_USE_IMAGE,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientMergeBase)) ==
                  LAYER_COMPOSITION_OPERATIONS_MERGE_BASE,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientMergeSrc)) ==
                  LAYER_COMPOSITION_OPERATIONS_MERGE_SRC,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientFrameScale)) ==
                  LAYER_COMPOSITION_OPERATIONS_FRAME_SCALE,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientSrcFrame)) ==
                  LAYER_COMPOSITION_OPERATIONS_SRC_FRAME,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientTransform)) ==
                  LAYER_COMPOSITION_OPERATIONS_TRANSFORM,
              "Const mismatch");
static_assert(
    (1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientColorConversion)) ==
        LAYER_COMPOSITION_OPERATIONS_COLOR_CONVERSION,
    "Const mismatch");
static_assert((1 << static_cast<int>(fhdt::wire::ClientCompositionOpcode::kClientAlpha)) ==
                  LAYER_COMPOSITION_OPERATIONS_ALPHA,
              "Const mismatch");

}  // namespace
