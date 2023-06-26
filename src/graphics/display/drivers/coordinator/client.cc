// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/client.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fit/defer.h>
#include <lib/image-format/image_format.h>
#include <lib/stdcompat/span.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/channel.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <math.h>
#include <string.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>
#include <cinttypes>
#include <cstdint>
#include <memory>
#include <random>
#include <type_traits>
#include <utility>
#include <vector>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/migration-util.h"
#include "src/graphics/display/lib/api-types-cpp/buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-layer-id.h"
#include "src/graphics/display/lib/api-types-cpp/layer-id.h"
#include "src/graphics/display/lib/edid/edid.h"
#include "src/lib/fsl/handles/object_info.h"

namespace fhd = fuchsia_hardware_display;

namespace {

constexpr uint32_t kFallbackHorizontalSizeMm = 160;
constexpr uint32_t kFallbackVerticalSizeMm = 90;

bool frame_contains(const frame_t& a, const frame_t& b) {
  return b.x_pos < a.width && b.y_pos < a.height && b.x_pos + b.width <= a.width &&
         b.y_pos + b.height <= a.height;
}

// We allocate some variable sized stack allocations based on the number of
// layers, so we limit the total number of layers to prevent blowing the stack.
constexpr uint64_t kMaxLayers = 65536;

}  // namespace

namespace display {

void DisplayConfig::InitializeInspect(inspect::Node* parent) {
  static std::atomic_uint64_t inspect_count;
  node_ = parent->CreateChild(fbl::StringPrintf("display-config-%ld", inspect_count++).c_str());
  pending_layer_change_property_ = node_.CreateBool("pending_layer_change", pending_layer_change_);
  pending_apply_layer_change_property_ =
      node_.CreateBool("pending_apply_layer_change", pending_apply_layer_change_);
}

void Client::ImportImage(ImportImageRequestView request, ImportImageCompleter::Sync& completer) {
  const display::BufferCollectionId buffer_collection_id =
      ToBufferCollectionId(request->collection_id);
  auto it = collection_map_.find(buffer_collection_id);
  if (it == collection_map_.end()) {
    completer.Reply(ZX_ERR_INVALID_ARGS);
    return;
  }
  const auto& collections = it->second;

  // Can't import an image with an id that's already in use.
  auto image_check = images_.find(request->image_id);
  if (image_check.IsValid()) {
    completer.Reply(ZX_ERR_ALREADY_EXISTS);
    return;
  }

  image_t dc_image = {};
  dc_image.height = request->image_config.height;
  dc_image.width = request->image_config.width;
  dc_image.type = request->image_config.type;

  const uint64_t banjo_driver_buffer_collection_id =
      display::ToBanjoDriverBufferCollectionId(collections.driver_buffer_collection_id);
  zx_status_t status =
      controller_->dc()->ImportImage(&dc_image, banjo_driver_buffer_collection_id, request->index);
  if (status != ZX_OK) {
    completer.Reply(status);
    return;
  }

  auto release_image =
      fit::defer([this, &dc_image]() { controller_->dc()->ReleaseImage(&dc_image); });
  zx::vmo vmo;

  fbl::AllocChecker ac;
  auto image =
      fbl::AdoptRef(new (&ac) Image(controller_, dc_image, std::move(vmo), &proxy_->node(), id_));
  if (!ac.check()) {
    zxlogf(DEBUG, "Alloc checker failed while constructing Image.\n");
    completer.Reply(ZX_ERR_NO_MEMORY);
    return;
  }

  auto image_id = request->image_id;
  image->id = image_id;
  release_image.cancel();
  images_.insert(std::move(image));

  completer.Reply(0);
}

void Client::ReleaseImage(ReleaseImageRequestView request,
                          ReleaseImageCompleter::Sync& /*_completer*/) {
  auto image = images_.find(request->image_id);
  if (!image.IsValid()) {
    return;
  }

  if (CleanUpImage(*image)) {
    ApplyConfig();
  }
}

void Client::ImportEvent(ImportEventRequestView request,
                         ImportEventCompleter::Sync& /*_completer*/) {
  if (request->id == INVALID_ID) {
    zxlogf(ERROR, "Cannot import events with an invalid ID #%i", INVALID_ID);
    TearDown();
  } else if (fences_.ImportEvent(std::move(request->event), request->id) != ZX_OK) {
    TearDown();
  }
}

void Client::ImportBufferCollection(ImportBufferCollectionRequestView request,
                                    ImportBufferCollectionCompleter::Sync& completer) {
  if (!sysmem_allocator_.is_valid()) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  const display::BufferCollectionId buffer_collection_id =
      ToBufferCollectionId(request->collection_id);
  // TODO: Switch to .contains() when C++20.
  if (collection_map_.count(buffer_collection_id)) {
    completer.Reply(ZX_ERR_INVALID_ARGS);
    return;
  }

  const display::DriverBufferCollectionId driver_buffer_collection_id =
      controller_->GetNextDriverBufferCollectionId();
  const uint64_t banjo_driver_buffer_collection_id =
      display::ToBanjoDriverBufferCollectionId(driver_buffer_collection_id);
  zx_status_t import_status = controller_->dc()->ImportBufferCollection(
      banjo_driver_buffer_collection_id, request->collection_token.TakeChannel());
  if (import_status != ZX_OK) {
    zxlogf(WARNING, "Cannot import BufferCollection to display driver: %s",
           zx_status_get_string(import_status));
    completer.Reply(ZX_ERR_INTERNAL);
  }

  collection_map_[buffer_collection_id] = Collections{
      .driver_buffer_collection_id = driver_buffer_collection_id,
  };
  completer.Reply(ZX_OK);
}

void Client::ReleaseBufferCollection(ReleaseBufferCollectionRequestView request,
                                     ReleaseBufferCollectionCompleter::Sync& /*_completer*/) {
  const display::BufferCollectionId buffer_collection_id =
      ToBufferCollectionId(request->collection_id);
  auto it = collection_map_.find(buffer_collection_id);
  if (it == collection_map_.end()) {
    return;
  }

  const uint64_t banjo_driver_buffer_collection_id =
      display::ToBanjoDriverBufferCollectionId(it->second.driver_buffer_collection_id);
  // TODO(fxbug.dev/97955) Consider handling the error instead of ignoring it.
  controller_->dc()->ReleaseBufferCollection(banjo_driver_buffer_collection_id);

  collection_map_.erase(it);
}

void Client::SetBufferCollectionConstraints(
    SetBufferCollectionConstraintsRequestView request,
    SetBufferCollectionConstraintsCompleter::Sync& completer) {
  const display::BufferCollectionId buffer_collection_id =
      ToBufferCollectionId(request->collection_id);
  auto it = collection_map_.find(buffer_collection_id);
  if (it == collection_map_.end()) {
    completer.Reply(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& collections = it->second;

  image_t dc_image;
  dc_image.height = request->config.height;
  dc_image.width = request->config.width;
  dc_image.type = request->config.type;

  zx_status_t status = ZX_ERR_INTERNAL;

  const uint64_t banjo_driver_buffer_collection_id =
      display::ToBanjoDriverBufferCollectionId(collections.driver_buffer_collection_id);
  status = controller_->dc()->SetBufferCollectionConstraints(&dc_image,
                                                             banjo_driver_buffer_collection_id);
  if (status != ZX_OK) {
    zxlogf(WARNING,
           "Cannot set BufferCollection constraints using imported buffer collection (id=%lu) %s.",
           request->collection_id, zx_status_get_string(status));
    completer.Reply(ZX_ERR_INTERNAL);
  }
  completer.Reply(status);
}

void Client::ReleaseEvent(ReleaseEventRequestView request,
                          ReleaseEventCompleter::Sync& /*_completer*/) {
  fences_.ReleaseEvent(request->id);
}

void Client::CreateLayer(CreateLayerCompleter::Sync& completer) {
  // TODO(fxbug.dev/129036): Layer IDs should be client-managed.

  if (layers_.size() == kMaxLayers) {
    completer.Reply(ZX_ERR_NO_RESOURCES, 0);
    return;
  }

  fbl::AllocChecker alloc_checker;
  DriverLayerId driver_layer_id = next_driver_layer_id++;
  auto new_layer = fbl::make_unique_checked<Layer>(&alloc_checker, driver_layer_id);
  if (!alloc_checker.check()) {
    --driver_layer_id;
    completer.Reply(ZX_ERR_NO_MEMORY, 0);
    return;
  }

  layers_.insert(std::move(new_layer));

  // Driver-side layer IDs are currently exposed to coordinator clients.
  // fxbug.dev/129036 tracks having client-managed IDs. When that happens,
  // Client instances will be responsible for translating between driver-side
  // and client-side IDs.
  LayerId layer_id(driver_layer_id.value());
  completer.Reply(ZX_OK, ToFidlLayerId(layer_id));
}

void Client::DestroyLayer(DestroyLayerRequestView request,
                          DestroyLayerCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());

  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    zxlogf(ERROR, "Tried to destroy invalid layer %ld", request->layer_id);
    TearDown();
    return;
  }
  if (layer->in_use()) {
    zxlogf(ERROR, "Destroyed layer %ld which was in use", request->layer_id);
    TearDown();
    return;
  }

  layers_.erase(driver_layer_id);
}

void Client::SetDisplayMode(SetDisplayModeRequestView request,
                            SetDisplayModeCompleter::Sync& /*_completer*/) {
  const DisplayId display_id = ToDisplayId(request->display_id);
  auto config = configs_.find(display_id);
  if (!config.IsValid()) {
    return;
  }

  fbl::AutoLock lock(controller_->mtx());
  const fbl::Vector<edid::timing_params_t>* edid_timings;
  const display_params_t* params;
  controller_->GetPanelConfig(display_id, &edid_timings, &params);

  if (edid_timings) {
    for (auto timing : *edid_timings) {
      if (timing.horizontal_addressable == request->mode.horizontal_resolution &&
          timing.vertical_addressable == request->mode.vertical_resolution &&
          timing.vertical_refresh_e2 == request->mode.refresh_rate_e2) {
        Controller::PopulateDisplayMode(timing, &config->pending_.mode);
        pending_config_valid_ = false;
        config->display_config_change_ = true;
        return;
      }
    }
    zxlogf(ERROR, "Invalid display mode");
  } else {
    zxlogf(ERROR, "Failed to find edid when setting display mode");
  }

  TearDown();
}

void Client::SetDisplayColorConversion(SetDisplayColorConversionRequestView request,
                                       SetDisplayColorConversionCompleter::Sync& /*_completer*/) {
  const DisplayId display_id = ToDisplayId(request->display_id);
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
  const DisplayId display_id = ToDisplayId(request->display_id);
  auto config = configs_.find(display_id);
  if (!config.IsValid()) {
    return;
  }

  config->pending_layer_change_ = true;
  config->pending_layer_change_property_.Set(true);
  config->pending_layers_.clear();
  for (uint64_t i = request->layer_ids.count() - 1; i != UINT64_MAX; i--) {
    LayerId layer_id = ToLayerId(request->layer_ids[i]);

    // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
    // driver-side ID will have to be looked up in a map.
    DriverLayerId driver_layer_id(layer_id.value());
    auto layer = layers_.find(driver_layer_id);
    if (!layer.IsValid()) {
      zxlogf(ERROR, "Unknown layer %lu", request->layer_ids[i]);
      TearDown();
      return;
    }

    // The cast is lossless because FIDL vector size is capped at 2^32-1.
    const uint32_t z_index = static_cast<uint32_t>(i);

    if (!layer->AddToConfig(&config->pending_layers_, z_index)) {
      zxlogf(ERROR, "Tried to reuse an in-use layer");
      TearDown();
      return;
    }
  }
  config->pending_.layer_count = static_cast<int32_t>(request->layer_ids.count());
  pending_config_valid_ = false;
}

void Client::SetLayerPrimaryConfig(SetLayerPrimaryConfigRequestView request,
                                   SetLayerPrimaryConfigCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    zxlogf(ERROR, "SetLayerPrimaryConfig on invalid layer");
    TearDown();
    return;
  }

  layer->SetPrimaryConfig(request->image_config);
  pending_config_valid_ = false;
  // no Reply defined
}

void Client::SetLayerPrimaryPosition(SetLayerPrimaryPositionRequestView request,
                                     SetLayerPrimaryPositionCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid() || layer->pending_type() != LAYER_TYPE_PRIMARY) {
    zxlogf(ERROR, "SetLayerPrimaryPosition on invalid layer");
    TearDown();
    return;
  }
  if (request->transform > fhd::wire::Transform::kRot90ReflectY) {
    zxlogf(ERROR, "Invalid transform %hhu", static_cast<uint8_t>(request->transform));
    TearDown();
    return;
  }
  layer->SetPrimaryPosition(request->transform, request->src_frame, request->dest_frame);
  pending_config_valid_ = false;
  // no Reply defined
}

void Client::SetLayerPrimaryAlpha(SetLayerPrimaryAlphaRequestView request,
                                  SetLayerPrimaryAlphaCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid() || layer->pending_type() != LAYER_TYPE_PRIMARY) {
    zxlogf(ERROR, "SetLayerPrimaryAlpha on invalid layer");
    TearDown();
    return;
  }

  if (request->mode > fhd::wire::AlphaMode::kHwMultiply ||
      (!isnan(request->val) && (request->val < 0 || request->val > 1))) {
    zxlogf(ERROR, "Invalid args %hhu %f", static_cast<uint8_t>(request->mode), request->val);
    TearDown();
    return;
  }
  layer->SetPrimaryAlpha(request->mode, request->val);
  pending_config_valid_ = false;
  // no Reply defined
}

void Client::SetLayerCursorConfig(SetLayerCursorConfigRequestView request,
                                  SetLayerCursorConfigCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    zxlogf(ERROR, "SetLayerCursorConfig on invalid layer");
    TearDown();
    return;
  }

  layer->SetCursorConfig(request->image_config);
  pending_config_valid_ = false;
  // no Reply defined
}

void Client::SetLayerCursorPosition(SetLayerCursorPositionRequestView request,
                                    SetLayerCursorPositionCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid() || layer->pending_type() != LAYER_TYPE_CURSOR) {
    zxlogf(ERROR, "SetLayerCursorPosition on invalid layer");
    TearDown();
    return;
  }

  layer->SetCursorPosition(request->x, request->y);
  // no Reply defined
}

void Client::SetLayerColorConfig(SetLayerColorConfigRequestView request,
                                 SetLayerColorConfigCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    zxlogf(ERROR, "SetLayerColorConfig on invalid layer");
    return;
  }

  uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(PixelFormatAndModifier(
      request->pixel_format,
      /*pixel_format_modifier_param=*/fuchsia_images2::wire::kFormatModifierLinear));
  if (request->color_bytes.count() != bytes_per_pixel) {
    zxlogf(ERROR, "SetLayerColorConfig with invalid color bytes");
    TearDown();
    return;
  }

  layer->SetColorConfig(request->pixel_format, request->color_bytes);
  pending_config_valid_ = false;
  // no Reply defined
}

void Client::SetLayerImage(SetLayerImageRequestView request,
                           SetLayerImageCompleter::Sync& /*_completer*/) {
  LayerId layer_id = ToLayerId(request->layer_id);

  // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
  // driver-side ID will have to be looked up in a map.
  DriverLayerId driver_layer_id(layer_id.value());
  auto layer = layers_.find(driver_layer_id);
  if (!layer.IsValid()) {
    zxlogf(ERROR, "SetLayerImage ordinal with invalid layer %lu", request->layer_id);
    TearDown();
    return;
  }
  if (layer->pending_type() != LAYER_TYPE_PRIMARY && layer->pending_type() != LAYER_TYPE_CURSOR) {
    zxlogf(ERROR, "SetLayerImage ordinal with bad layer type");
    TearDown();
    return;
  }

  auto image = images_.find(request->image_id);
  if (!image.IsValid() || !image->Acquire()) {
    zxlogf(ERROR, "SetLayerImage ordinal with %s image", !image.IsValid() ? "invl" : "busy");
    TearDown();
    return;
  }
  const image_t* cur_image = layer->pending_image();
  // TODO(fxbug.dev/126156): Warning: Currently we only compare size and usage
  // type between `image` and `layer`. This implicitly assume that images can
  // be applied to any layer as long as the format is negotiated by sysmem,
  // which may not be true in the future. We should figure out a way to better
  // indicate pixel format support of a Layer in display Controller API.
  if (!image->HasSameDisplayPropertiesAsLayer(*cur_image)) {
    zxlogf(ERROR, "SetLayerImage with mismatch layer config");
    if (image.IsValid()) {
      image->DiscardAcquire();
    }
    TearDown();
    return;
  }

  layer->SetImage(image.CopyPointer(), request->wait_event_id, request->signal_event_id);
  // no Reply defined
}

void Client::CheckConfig(CheckConfigRequestView request, CheckConfigCompleter::Sync& completer) {
  fhd::wire::ConfigResult res;
  std::vector<fhd::wire::ClientCompositionOp> ops;

  pending_config_valid_ = CheckConfig(&res, &ops);

  if (request->discard) {
    // Go through layers and release any pending resources they claimed
    for (auto& layer : layers_) {
      layer.DiscardChanges();
    }
    // Reset pending layers lists of all displays to their current layers
    // respectively.
    SetAllConfigPendingLayersToCurrentLayers();
    for (auto& config : configs_) {
      config.pending_layer_change_ = false;
      config.pending_layer_change_property_.Set(false);

      config.pending_ = config.current_;
      config.display_config_change_ = false;
    }
    pending_config_valid_ = true;
  }

  completer.Reply(res, ::fidl::VectorView<fhd::wire::ClientCompositionOp>::FromExternal(ops));
}

void Client::ApplyConfig(ApplyConfigCompleter::Sync& /*_completer*/) {
  if (!pending_config_valid_) {
    pending_config_valid_ = CheckConfig(nullptr, nullptr);
    if (!pending_config_valid_) {
      zxlogf(INFO, "Tried to apply invalid config");
      return;
    }
  }

  // Now that we can guarantee that the configuration will be applied, it is
  // safe to increment the config stamp counter.
  ++latest_config_stamp_;

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
        zxlogf(ERROR, "Failed to resolve pending layer properties for layer %" PRIu64,
               layer_node.layer->id.value());
        TearDown();
        return;
      }
      if (!layer_node.layer->ResolvePendingImage(&fences_, latest_config_stamp_)) {
        zxlogf(ERROR, "Failed to resolve pending images for layer %" PRIu64,
               layer_node.layer->id.value());
        TearDown();
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
        // Controller::ApplyConfig for more details.
        if (layer->current_display_id_ != display_config.id && layer->displayed_image_ &&
            !layer->waiting_images_.is_empty()) {
          {
            fbl::AutoLock lock(controller_->mtx());
            controller_->AssertMtxAliasHeld(layer->displayed_image_->mtx());
            layer->displayed_image_->StartRetire();
          }
          layer->displayed_image_ = nullptr;

          // This doesn't need to be reset anywhere, since we really care about the last
          // display this layer was shown on. Ignoring the 'null' display could cause
          // unusual layer changes to trigger this unnecessary, but that's not wrong.
          layer->current_display_id_ = display_config.id;
        }
        layer->current_layer_.z_index = layer->pending_layer_.z_index;
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

  // no Reply defined
}

void Client::GetLatestAppliedConfigStamp(GetLatestAppliedConfigStampCompleter::Sync& completer) {
  completer.Reply(ToFidlConfigStamp(latest_config_stamp_));
}

void Client::EnableVsync(EnableVsyncRequestView request,
                         EnableVsyncCompleter::Sync& /*_completer*/) {
  proxy_->EnableVsync(request->enable);
  // no Reply defined
}

void Client::SetVirtconMode(SetVirtconModeRequestView request,
                            SetVirtconModeCompleter::Sync& /*_completer*/) {
  if (!is_vc_) {
    zxlogf(ERROR, "Illegal non-virtcon ownership");
    TearDown();
    return;
  }
  controller_->SetVcMode(request->mode);
  // no Reply defined
}

void Client::IsCaptureSupported(IsCaptureSupportedCompleter::Sync& completer) {
  completer.ReplySuccess(controller_->supports_capture());
}

void Client::ImportImageForCapture(ImportImageForCaptureRequestView request,
                                   ImportImageForCaptureCompleter::Sync& completer) {
  // Ensure display driver supports/implements capture.
  if (!controller_->supports_capture()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  // Ensure a previously imported collection id is being used for import.
  const display::BufferCollectionId buffer_collection_id =
      ToBufferCollectionId(request->collection_id);
  auto it = collection_map_.find(buffer_collection_id);
  if (it == collection_map_.end()) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  const auto& collections = it->second;
  const uint64_t banjo_driver_buffer_collection_id =
      display::ToBanjoDriverBufferCollectionId(collections.driver_buffer_collection_id);

  uint64_t capture_image_handle = INVALID_ID;
  zx_status_t status = controller_->dc()->ImportImageForCapture(
      banjo_driver_buffer_collection_id, request->index, &capture_image_handle);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  auto release_image = fit::defer(
      [this, capture_image_handle]() { controller_->dc()->ReleaseCapture(capture_image_handle); });

  fbl::AllocChecker alloc_checker;
  fbl::RefPtr<CaptureImage> capture_image = fbl::AdoptRef(
      new (&alloc_checker) CaptureImage(controller_, capture_image_handle, &proxy_->node(), id_));
  if (!alloc_checker.check()) {
    completer.ReplyError(ZX_ERR_NO_MEMORY);
    return;
  }
  // `capture_image_handle` is now owned by the CaptureImage instance.
  release_image.cancel();

  const uint64_t capture_image_id = next_capture_image_id_++;
  capture_image->id = capture_image_id;
  capture_images_.insert(std::move(capture_image));
  completer.ReplySuccess(capture_image_id);
}

void Client::StartCapture(StartCaptureRequestView request, StartCaptureCompleter::Sync& completer) {
  // Ensure display driver supports/implements capture.
  if (!controller_->supports_capture()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  // Don't start capture if one is in progress
  if (current_capture_image_ != INVALID_ID) {
    completer.ReplyError(ZX_ERR_SHOULD_WAIT);
    return;
  }

  // Ensure we have a capture fence for the request signal event.
  auto signal_fence = fences_.GetFence(request->signal_event_id);
  if (signal_fence == nullptr) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  // Ensure we are capturing into a valid image buffer
  auto image = capture_images_.find(request->image_id);
  if (!image.IsValid()) {
    zxlogf(ERROR, "Invalid Capture Image ID requested for capture");
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  capture_fence_id_ = request->signal_event_id;
  auto status = controller_->dc()->StartCapture(image->capture_image_handle());
  if (status == ZX_OK) {
    fbl::AutoLock lock(controller_->mtx());
    proxy_->EnableCapture(true);
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }

  // keep track of currently active capture image
  current_capture_image_ = request->image_id;  // Is this right?
}

void Client::ReleaseCapture(ReleaseCaptureRequestView request,
                            ReleaseCaptureCompleter::Sync& completer) {
  // Ensure display driver supports/implements capture
  if (!controller_->supports_capture()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  // Ensure we are releasing a valid image buffer
  auto image = capture_images_.find(request->image_id);
  if (!image.IsValid()) {
    zxlogf(ERROR, "Invalid Capture Image ID requested for release");
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  // Make sure we are not releasing an active capture.
  if (current_capture_image_ == request->image_id) {
    // we have an active capture. Release it when capture is completed
    zxlogf(WARNING, "Capture is active. Will release after capture is complete");
    pending_capture_release_image_ = current_capture_image_;
  } else {
    // release image now
    capture_images_.erase(image);
  }
  completer.ReplySuccess();
}

void Client::SetMinimumRgb(SetMinimumRgbRequestView request,
                           SetMinimumRgbCompleter::Sync& completer) {
  if (controller_->dc_clamp_rgb() == nullptr) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  if (!is_owner_) {
    completer.ReplyError(ZX_ERR_NOT_CONNECTED);
    return;
  }
  auto status = controller_->dc_clamp_rgb()->SetMinimumRgb(request->minimum_rgb);
  if (status == ZX_OK) {
    client_minimum_rgb_ = request->minimum_rgb;
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void Client::SetDisplayPower(SetDisplayPowerRequestView request,
                             SetDisplayPowerCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(controller_->dc());
  auto status = controller_->dc()->SetDisplayPower(request->display_id, request->power_on);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

bool Client::CheckConfig(fhd::wire::ConfigResult* res,
                         std::vector<fhd::wire::ClientCompositionOp>* ops) {
  if (res && ops) {
    *res = fhd::wire::ConfigResult::kOk;
    ops->clear();
  }
  if (configs_.size() == 0) {
    // An empty config is always valid.
    return true;
  }
  const size_t layers_size = std::max(static_cast<size_t>(1), layers_.size());
  const display_config_t* configs[configs_.size()];
  layer_t* layers[layers_size];
  uint32_t layer_cfg_results[layers_size];
  uint32_t* display_layer_cfg_results[configs_.size()];
  memset(layer_cfg_results, 0, layers_size * sizeof(uint32_t));

  bool config_fail = false;
  size_t config_idx = 0;
  size_t layer_idx = 0;
  for (auto& display_config : configs_) {
    if (display_config.pending_layers_.is_empty()) {
      continue;
    }

    // Put this display's display_config_t* into the compact array
    configs[config_idx] = &display_config.pending_;

    // Set the index in the primary result array with this display's layer result array
    display_layer_cfg_results[config_idx++] = layer_cfg_results + layer_idx;

    // Create this display's compact layer_t* array
    display_config.pending_.layer_list = layers + layer_idx;

    // Frame used for checking that each layer's dest_frame lies entirely
    // within the composed output.
    frame_t display_frame = {
        .x_pos = 0,
        .y_pos = 0,
        .width = display_config.pending_.mode.h_addressable,
        .height = display_config.pending_.mode.v_addressable,
    };

    // Do any work that needs to be done to make sure that the pending layer_t structs
    // are up to date, and validate that the configuration doesn't violate any API
    // constraints.
    for (auto& layer_node : display_config.pending_layers_) {
      layers[layer_idx++] = &layer_node.layer->pending_layer_;

      bool invalid = false;
      if (layer_node.layer->pending_layer_.type == LAYER_TYPE_PRIMARY) {
        primary_layer_t* layer = &layer_node.layer->pending_layer_.cfg.primary;
        // Frame for checking that the layer's src_frame lies entirely
        // within the source image.
        frame_t image_frame = {
            .x_pos = 0,
            .y_pos = 0,
            .width = layer->image.width,
            .height = layer->image.height,
        };
        invalid = (!frame_contains(image_frame, layer->src_frame) ||
                   !frame_contains(display_frame, layer->dest_frame));
        // The formats of layer images are negotiated by sysmem between clients
        // and display engine drivers when being imported, so they are always
        // accepted by the display coordinator.
      } else if (layer_node.layer->pending_layer_.type == LAYER_TYPE_CURSOR) {
        // TODO(fxbug.dev/126123): Currently we don't check the pixel formats,
        // nor sizes of the imported image. These should be done when we import
        // cursor images to the display driver, and the clients should only use
        // images imported for cursor use on cursor layers.
        invalid = false;
      } else if (layer_node.layer->pending_layer_.type == LAYER_TYPE_COLOR) {
        // There aren't any API constraints on valid colors.
        layer_node.layer->pending_layer_.cfg.color.color_list =
            layer_node.layer->pending_color_bytes_;
        layer_node.layer->pending_layer_.cfg.color.color_count = 4;
      } else {
        invalid = true;
      }

      if (invalid) {
        // Continue to the next display, since there's nothing more to check for this one.
        config_fail = true;
        break;
      }
    }
  }

  if (config_fail) {
    if (res) {
      *res = fhd::wire::ConfigResult::kInvalidConfig;
    }
    // If the config is invalid, there's no point in sending it to the impl driver.
    return false;
  }

  size_t layer_cfg_results_count;
  uint32_t display_cfg_result = controller_->dc()->CheckConfiguration(
      configs, config_idx, display_layer_cfg_results, &layer_cfg_results_count);

  if (display_cfg_result != CONFIG_CHECK_RESULT_OK) {
    if (res) {
      *res = display_cfg_result == CONFIG_CHECK_RESULT_TOO_MANY
                 ? fhd::wire::ConfigResult::kTooManyDisplays
                 : fhd::wire::ConfigResult::kUnsupportedDisplayModes;
    }
    return false;
  }

  bool layer_fail = false;
  for (size_t i = 0; i < config_idx && !layer_fail; i++) {
    for (unsigned j = 0; j < configs[i]->layer_count && !layer_fail; j++) {
      if (display_layer_cfg_results[i][j]) {
        layer_fail = true;
      }
    }
  }

  // Return unless we need to finish constructing the response
  if (!layer_fail) {
    return true;
  }
  if (!(res && ops)) {
    return false;
  }
  *res = fhd::wire::ConfigResult::kUnsupportedConfig;

  // TODO(b/249297195): Once Gerrit IFTTT supports multiple paths, add IFTTT
  // comments to make sure that any change of type Client in
  // //sdk/banjo/fuchsia.hardware.display.controller/display-controller.fidl
  // will cause the definition of `kAllErrors` to change as well.
  constexpr uint32_t kAllErrors = CLIENT_USE_PRIMARY | CLIENT_MERGE_BASE | CLIENT_MERGE_SRC |
                                  CLIENT_FRAME_SCALE | CLIENT_SRC_FRAME | CLIENT_TRANSFORM |
                                  CLIENT_COLOR_CONVERSION | CLIENT_ALPHA;

  layer_idx = 0;
  for (auto& display_config : configs_) {
    if (display_config.pending_layers_.is_empty()) {
      continue;
    }

    bool seen_base = false;
    for (auto& layer_node : display_config.pending_layers_) {
      uint32_t err = kAllErrors & layer_cfg_results[layer_idx];
      // Fixup the error flags if the driver impl incorrectly set multiple MERGE_BASEs
      if (err & CLIENT_MERGE_BASE) {
        if (seen_base) {
          err &= ~CLIENT_MERGE_BASE;
          err |= CLIENT_MERGE_SRC;
        } else {
          seen_base = true;
          err &= ~CLIENT_MERGE_SRC;
        }
      }

      const uint64_t fidl_display_id = ToFidlDisplayId(display_config.id);

      // TODO(fxbug.dev/192036): When switching to client-managed IDs, the
      // client-side ID will have to be looked up in a map.
      LayerId layer_id(layer_node.layer->id.value());
      const uint64_t fidl_layer_id = ToFidlLayerId(layer_id);

      for (uint8_t i = 0; i < 32; i++) {
        if (err & (1 << i)) {
          ops->emplace_back(fhd::wire::ClientCompositionOp{
              .display_id = fidl_display_id,
              .layer_id = fidl_layer_id,
              .opcode = static_cast<fhd::wire::ClientCompositionOpcode>(i),
          });
        }
      }
      layer_idx++;
    }
  }
  return false;
}

void Client::ApplyConfig() {
  ZX_DEBUG_ASSERT(controller_->current_thread_is_loop());
  TRACE_DURATION("gfx", "Display::Client::ApplyConfig");

  bool config_missing_image = false;
  // Clients can apply zero-layer configs. Ensure that the VLA is at least 1 element long.
  layer_t* layers[layers_.size() + 1];
  int layer_idx = 0;

  // Layers may have pending images, and it is possible that a layer still
  // uses images from previous configurations. We should take this into account
  // when sending the config_stamp to |Controller|.
  //
  // We keep track of the "current client config stamp" for each image, the
  // value of which is only updated when a configuration uses an image that is
  // ready on application, or when the image's wait fence has been signaled and
  // |ActivateLatestReadyImage()| activates the new image.
  //
  // The final config_stamp sent to |Controller| will be the minimum of all
  // per-layer stamps.
  ConfigStamp current_applied_config_stamp = latest_config_stamp_;

  for (auto& display_config : configs_) {
    display_config.current_.layer_count = 0;
    display_config.current_.layer_list = layers + layer_idx;

    // Displays with no current layers are filtered out in Controller::ApplyConfig,
    // after it updates its own image tracking logic.

    for (auto& layer_node : display_config.current_layers_) {
      Layer* layer = layer_node.layer;
      const bool activated = layer->ActivateLatestReadyImage();
      if (activated && layer->current_image()) {
        display_config.pending_apply_layer_change_ = true;
        display_config.pending_apply_layer_change_property_.Set(true);
      }

      std::optional<ConfigStamp> layer_client_config_stamp = layer->GetCurrentClientConfigStamp();
      if (layer_client_config_stamp) {
        current_applied_config_stamp =
            std::min(current_applied_config_stamp, *layer_client_config_stamp);
      }

      display_config.current_.layer_count++;
      layers[layer_idx++] = &layer->current_layer_;
      if (layer->current_layer_.type != LAYER_TYPE_COLOR) {
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

    controller_->ApplyConfig(dc_configs, dc_idx, is_vc_, current_applied_config_stamp,
                             client_apply_count_, id_);
  }
}

void Client::SetOwnership(bool is_owner) {
  ZX_DEBUG_ASSERT(controller_->current_thread_is_loop());
  is_owner_ = is_owner;

  fidl::Status result = binding_state_.SendEvents([&](auto&& endpoint) {
    return fidl::WireSendEvent(endpoint)->OnClientOwnershipChange(is_owner);
  });
  if (!result.ok()) {
    zxlogf(ERROR, "Error writing remove message: %s", result.FormatDescription().c_str());
  }

  // Only apply the current config if the client has previously applied a config.
  if (client_apply_count_) {
    ApplyConfig();
  }
}

void Client::OnDisplaysChanged(cpp20::span<const DisplayId> added_display_ids,
                               cpp20::span<const DisplayId> removed_display_ids) {
  ZX_DEBUG_ASSERT(controller_->current_thread_is_loop());

  controller_->AssertMtxAliasHeld(controller_->mtx());
  for (DisplayId added_display_id : added_display_ids) {
    fbl::AllocChecker ac;
    auto config = fbl::make_unique_checked<DisplayConfig>(&ac);
    if (!ac.check()) {
      zxlogf(WARNING, "Out of memory when processing hotplug");
      continue;
    }

    config->id = added_display_id;

    zx::result get_supported_pixel_formats_result =
        controller_->GetSupportedPixelFormats(config->id);
    if (get_supported_pixel_formats_result.is_error()) {
      zxlogf(WARNING, "Failed to get pixel formats when processing hotplug: %s",
             get_supported_pixel_formats_result.status_string());
      continue;
    }
    config->pixel_formats_ = std::move(get_supported_pixel_formats_result.value());

    zx::result get_cursor_infos_result = controller_->GetCursorInfo(config->id);
    if (get_cursor_infos_result.is_error()) {
      zxlogf(WARNING, "Failed to get cursor info when processing hotplug: %s",
             get_cursor_infos_result.status_string());
      continue;
    }
    config->cursor_infos_ = std::move(get_cursor_infos_result.value());

    const fbl::Vector<edid::timing_params_t>* edid_timings;
    const display_params_t* params;
    if (!controller_->GetPanelConfig(config->id, &edid_timings, &params)) {
      // This can only happen if the display was already disconnected.
      zxlogf(WARNING, "No config when adding display");
      continue;
    }

    config->current_.display_id = ToBanjoDisplayId(config->id);
    config->current_.layer_list = nullptr;
    config->current_.layer_count = 0;

    if (edid_timings) {
      Controller::PopulateDisplayMode((*edid_timings)[0], &config->current_.mode);
    } else {
      config->current_.mode = {};
      config->current_.mode.h_addressable = params->width;
      config->current_.mode.v_addressable = params->height;
    }

    config->current_.cc_flags = 0;

    config->pending_ = config->current_;

    config->InitializeInspect(&proxy_->node());

    configs_.insert(std::move(config));
  }

  // We need 2 loops, since we need to make sure we allocate the
  // correct size array in the fidl response.
  std::vector<fhd::wire::Info> coded_configs;
  coded_configs.reserve(added_display_ids.size());

  // Hang on to modes values until we send the message.
  std::vector<std::vector<fhd::wire::Mode>> modes_vector;

  fidl::Arena arena;
  for (DisplayId added_display_id : added_display_ids) {
    auto config = configs_.find(added_display_id);
    if (!config.IsValid()) {
      continue;
    }

    fhd::wire::Info info;
    info.id = ToFidlDisplayId(config->id);

    const fbl::Vector<edid::timing_params>* edid_timings;
    const display_params_t* params;
    controller_->GetPanelConfig(config->id, &edid_timings, &params);
    std::vector<fhd::wire::Mode> modes;
    if (edid_timings) {
      modes.reserve(edid_timings->size());
      for (auto timing : *edid_timings) {
        modes.emplace_back(fhd::wire::Mode{
            .horizontal_resolution = timing.horizontal_addressable,
            .vertical_resolution = timing.vertical_addressable,
            .refresh_rate_e2 = timing.vertical_refresh_e2,
        });
      }
    } else {
      modes.reserve(1);
      modes.emplace_back(fhd::wire::Mode{
          .horizontal_resolution = params->width,
          .vertical_resolution = params->height,
          .refresh_rate_e2 = params->refresh_rate_e2,
      });
    }
    modes_vector.emplace_back(std::move(modes));
    info.modes = fidl::VectorView<fhd::wire::Mode>::FromExternal(modes_vector.back());

    info.pixel_format =
        fidl::VectorView<fuchsia_images2::wire::PixelFormat>(arena, config->pixel_formats_.size());
    for (size_t pixel_format_index = 0; pixel_format_index < info.pixel_format.count();
         ++pixel_format_index) {
      info.pixel_format[pixel_format_index] = config->pixel_formats_[pixel_format_index].ToFidl();
    }

    info.cursor_configs =
        fidl::VectorView<fhd::wire::CursorInfo>(arena, config->cursor_infos_.size());
    for (size_t cursor_config_index = 0; cursor_config_index < info.cursor_configs.count();
         ++cursor_config_index) {
      info.cursor_configs[cursor_config_index] =
          config->cursor_infos_[cursor_config_index].ToFidl();
    }

    const char* manufacturer_name = "";
    const char* monitor_name = "";
    const char* monitor_serial = "";
    if (!controller_->GetDisplayIdentifiers(added_display_id, &manufacturer_name, &monitor_name,
                                            &monitor_serial)) {
      zxlogf(ERROR, "Failed to get display identifiers");
      ZX_DEBUG_ASSERT(false);
    }

    info.using_fallback_size = false;
    if (!controller_->GetDisplayPhysicalDimensions(added_display_id, &info.horizontal_size_mm,
                                                   &info.vertical_size_mm)) {
      zxlogf(ERROR, "Failed to get display physical dimensions");
      ZX_DEBUG_ASSERT(false);
    }
    if (info.horizontal_size_mm == 0 || info.vertical_size_mm == 0) {
      info.horizontal_size_mm = kFallbackHorizontalSizeMm;
      info.vertical_size_mm = kFallbackVerticalSizeMm;
      info.using_fallback_size = true;
    }

    info.manufacturer_name = fidl::StringView::FromExternal(manufacturer_name);
    info.monitor_name = fidl::StringView::FromExternal(monitor_name);
    info.monitor_serial = fidl::StringView::FromExternal(monitor_serial);

    coded_configs.push_back(info);
  }

  std::vector<uint64_t> fidl_removed_display_ids;
  fidl_removed_display_ids.reserve(removed_display_ids.size());

  for (DisplayId removed_display_id : removed_display_ids) {
    auto display = configs_.erase(removed_display_id);
    if (display) {
      display->pending_layers_.clear();
      display->current_layers_.clear();
      fidl_removed_display_ids.push_back(ToFidlDisplayId(display->id));
    }
  }

  if (!coded_configs.empty() || !fidl_removed_display_ids.empty()) {
    fidl::Status result = binding_state_.SendEvents([&](auto&& endpoint) {
      return fidl::WireSendEvent(endpoint)->OnDisplaysChanged(
          fidl::VectorView<fhd::wire::Info>::FromExternal(coded_configs),
          fidl::VectorView<uint64_t>::FromExternal(fidl_removed_display_ids));
    });
    if (!result.ok()) {
      zxlogf(ERROR, "Error writing remove message: %s", result.FormatDescription().c_str());
    }
  }
}

void Client::OnFenceFired(FenceReference* fence) {
  bool new_image_ready = false;
  for (auto& layer : layers_) {
    for (Image& waiting_image : layer.waiting_images_) {
      new_image_ready |= waiting_image.OnFenceReady(fence);
    }
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
  if (pending_capture_release_image_ != INVALID_ID) {
    auto image = capture_images_.find(pending_capture_release_image_);
    if (image.IsValid()) {
      capture_images_.erase(image);
    }
    pending_capture_release_image_ = INVALID_ID;
  }
  current_capture_image_ = INVALID_ID;
}

void Client::TearDown() {
  ZX_DEBUG_ASSERT(controller_->current_thread_is_loop());
  pending_config_valid_ = false;

  // Teardown stops events from the channel, but not from the ddk, so we
  // need to make sure we don't try to teardown multiple times.
  if (!IsValid()) {
    return;
  }

  // make sure we stop vsync messages from this client since the server end has already been closed
  // by the fidl server.
  proxy_->EnableVsync(false);

  running_ = false;

  CleanUpAllImages();
  zxlogf(INFO, "Releasing %lu capture images cur=%lu, pending=%lu", capture_images_.size(),
         current_capture_image_, pending_capture_release_image_);
  current_capture_image_ = pending_capture_release_image_ = INVALID_ID;
  capture_images_.clear();

  fences_.Clear();

  for (auto& config : configs_) {
    config.pending_layers_.clear();
    config.current_layers_.clear();
  }

  // The layer's images have already been handled in CleanUpImageLayerState
  layers_.clear();

  // Release all imported buffer collections on display drivers.
  for (const auto& [k, v] : collection_map_) {
    const uint64_t banjo_driver_buffer_collection_id =
        display::ToBanjoDriverBufferCollectionId(v.driver_buffer_collection_id);
    controller_->dc()->ReleaseBufferCollection(banjo_driver_buffer_collection_id);
  }
  collection_map_.clear();

  ApplyConfig();
}

void Client::TearDownTest() { running_ = false; }

bool Client::CleanUpAllImages() {
  // Clean up all image fences.
  {
    fbl::AutoLock lock(controller_->mtx());
    for (auto& image : images_) {
      controller_->AssertMtxAliasHeld(image.mtx());
      image.ResetFences();
    }
  }

  // Clean up any layer state associated with the images
  bool current_config_changed = std::any_of(layers_.begin(), layers_.end(),
                                            [](Layer& layer) { return layer.CleanUpAllImages(); });

  images_.clear();
  return current_config_changed;
}

bool Client::CleanUpImage(Image& image) {
  // Clean up any fences associated with the image
  {
    fbl::AutoLock lock(controller_->mtx());
    controller_->AssertMtxAliasHeld(image.mtx());
    image.ResetFences();
  }

  // Clean up any layer state associated with the images
  bool current_config_changed = std::any_of(
      layers_.begin(), layers_.end(),
      [&image = std::as_const(image)](Layer& layer) { return layer.CleanUpImage(image); });

  images_.erase(image);
  return current_config_changed;
}

void Client::CleanUpCaptureImage(uint64_t id) {
  if (id == INVALID_ID) {
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

void Client::AcknowledgeVsync(AcknowledgeVsyncRequestView request,
                              AcknowledgeVsyncCompleter::Sync& /*_completer*/) {
  acked_cookie_ = request->cookie;
  zxlogf(TRACE, "Cookie %ld Acked\n", request->cookie);
}

fpromise::result<fidl::ServerBindingRef<fuchsia_hardware_display::Coordinator>, zx_status_t>
Client::Init(fidl::ServerEnd<fuchsia_hardware_display::Coordinator> server_end) {
  running_ = true;

  fidl::OnUnboundFn<Client> cb = [](Client* client, fidl::UnbindInfo info,
                                    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> ch) {
    sync_completion_signal(client->fidl_unbound());
    // Make sure we TearDown() so that no further tasks are scheduled on the controller loop.
    client->TearDown();

    // The client has died so tell the Proxy which will free the classes.
    client->proxy_->OnClientDead();
  };

  auto binding = fidl::BindServer(controller_->loop().dispatcher(), std::move(server_end), this,
                                  std::move(cb));
  // Keep a copy of fidl binding so we can safely unbind from it during shutdown
  binding_state_.SetBound(binding);

  if (zx_status_t status =
          [&]() {
            zx::result endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
            if (endpoints.is_error()) {
              return endpoints.error_value();
            }
            auto& [sysmem_allocator_client, sysmem_allocator_server] = endpoints.value();
            if (zx_status_t status =
                    controller_->dc()->GetSysmemConnection(sysmem_allocator_server.TakeChannel());
                status != ZX_OK) {
              return status;
            }
            sysmem_allocator_ = fidl::WireSyncClient(std::move(sysmem_allocator_client));

            // TODO(fxbug.dev/97955) Consider handling the error instead of ignoring it.
            std::string debug_name = std::string("display[") + fsl::GetCurrentProcessName() + "]";
            fidl::OneWayStatus status = sysmem_allocator_->SetDebugClientInfo(
                fidl::StringView::FromExternal(debug_name), fsl::GetCurrentProcessKoid());
            return status.status();
          }();
      status != ZX_OK) {
    // Not a fatal error, but BufferCollection functions won't work.
    // TODO(fxbug.dev/33157) TODO: Fail creation once all drivers implement this.
    zxlogf(ERROR, "GetSysmemConnection failed (continuing) - status: %d", status);
  }

  return fpromise::ok(binding);
}

Client::Client(Controller* controller, ClientProxy* proxy, bool is_vc, uint32_t client_id)
    : controller_(controller),
      proxy_(proxy),
      is_vc_(is_vc),
      id_(client_id),
      fences_(controller->loop().dispatcher(), fit::bind_member<&Client::OnFenceFired>(this)) {}

Client::Client(Controller* controller, ClientProxy* proxy, bool is_vc, uint32_t client_id,
               fidl::ServerEnd<fhd::Coordinator> server_end)
    : controller_(controller),
      proxy_(proxy),
      is_vc_(is_vc),
      id_(client_id),
      running_(true),
      fences_(controller->loop().dispatcher(), fit::bind_member<&Client::OnFenceFired>(this)),
      binding_state_(std::move(server_end)) {}

Client::~Client() { ZX_DEBUG_ASSERT(!running_); }

void ClientProxy::SetOwnership(bool is_owner) {
  fbl::AllocChecker ac;
  auto task = fbl::make_unique_checked<async::Task>(&ac);
  if (!ac.check()) {
    zxlogf(WARNING, "Failed to allocate set ownership task");
    return;
  }
  task->set_handler([this, client_handler = &handler_, is_owner](
                        async_dispatcher_t* /*dispatcher*/, async::Task* task, zx_status_t status) {
    if (status == ZX_OK && client_handler->IsValid()) {
      is_owner_property_.Set(is_owner);
      client_handler->SetOwnership(is_owner);
    }
    // update client_scheduled_tasks_
    mtx_lock(&this->task_mtx_);
    auto it = std::find_if(client_scheduled_tasks_.begin(), client_scheduled_tasks_.end(),
                           [&](std::unique_ptr<async::Task>& t) { return t.get() == task; });
    // Current task must have been added to the list.
    ZX_DEBUG_ASSERT(it != client_scheduled_tasks_.end());
    client_scheduled_tasks_.erase(it);
    mtx_unlock(&this->task_mtx_);
  });
  mtx_lock(&task_mtx_);
  if (task->Post(controller_->loop().dispatcher()) == ZX_OK) {
    client_scheduled_tasks_.push_back(std::move(task));
  }
  mtx_unlock(&task_mtx_);
}

void ClientProxy::OnDisplaysChanged(cpp20::span<const DisplayId> added_display_ids,
                                    cpp20::span<const DisplayId> removed_display_ids) {
  handler_.OnDisplaysChanged(added_display_ids, removed_display_ids);
}

void ClientProxy::ReapplySpecialConfigs() {
  ZX_DEBUG_ASSERT(mtx_trylock(controller_->mtx()) == thrd_busy);
  if (controller_->dc_clamp_rgb()) {
    controller_->dc_clamp_rgb()->SetMinimumRgb(handler_.GetMinimumRgb());
  }
}

void ClientProxy::ReapplyConfig() {
  fbl::AllocChecker ac;
  auto task = fbl::make_unique_checked<async::Task>(&ac);
  if (!ac.check()) {
    zxlogf(WARNING, "Failed to reapply config");
    return;
  }

  task->set_handler([this, client_handler = &handler_](async_dispatcher_t* /*dispatcher*/,
                                                       async::Task* task, zx_status_t status) {
    if (status == ZX_OK && client_handler->IsValid()) {
      client_handler->ApplyConfig();
    }
    // update client_scheduled_tasks_
    mtx_lock(&this->task_mtx_);
    auto it = std::find_if(client_scheduled_tasks_.begin(), client_scheduled_tasks_.end(),
                           [&](std::unique_ptr<async::Task>& t) { return t.get() == task; });
    // Current task must have been added to the list.
    ZX_DEBUG_ASSERT(it != client_scheduled_tasks_.end());
    client_scheduled_tasks_.erase(it);
    mtx_unlock(&this->task_mtx_);
  });
  mtx_lock(&task_mtx_);
  if (task->Post(controller_->loop().dispatcher()) == ZX_OK) {
    client_scheduled_tasks_.push_back(std::move(task));
  }
  mtx_unlock(&task_mtx_);
}

zx_status_t ClientProxy::OnCaptureComplete() {
  ZX_DEBUG_ASSERT(mtx_trylock(controller_->mtx()) == thrd_busy);
  fbl::AutoLock l(&mtx_);
  if (enable_capture_) {
    handler_.CaptureCompleted();
  }
  enable_capture_ = false;
  return ZX_OK;
}

zx_status_t ClientProxy::OnDisplayVsync(DisplayId display_id, zx_time_t timestamp,
                                        ConfigStamp controller_stamp) {
  ZX_DEBUG_ASSERT(mtx_trylock(controller_->mtx()) == thrd_busy);
  fidl::Status event_sending_result = fidl::Status::Ok();

  ConfigStamp client_stamp = {};
  auto it =
      std::find_if(pending_applied_config_stamps_.begin(), pending_applied_config_stamps_.end(),
                   [controller_stamp](const ConfigStampPair& stamp) {
                     return stamp.controller_stamp >= controller_stamp;
                   });

  if (it == pending_applied_config_stamps_.end() || it->controller_stamp != controller_stamp) {
    client_stamp = kInvalidConfigStamp;
  } else {
    client_stamp = it->client_stamp;
    pending_applied_config_stamps_.erase(pending_applied_config_stamps_.begin(), it);
  }

  {
    fbl::AutoLock l(&mtx_);
    if (!enable_vsync_) {
      return ZX_ERR_NOT_SUPPORTED;
    }
  }

  uint64_t cookie = 0;
  if (number_of_vsyncs_sent_ >= (kVsyncMessagesWatermark - 1)) {
    // Number of  vsync events sent exceed the watermark level.
    // Check to see if client has been notified already that acknowledgement is needed
    if (!acknowledge_request_sent_) {
      // We have not sent a (new) cookie to client for acknowledgement. Let's do it now
      // First increment cookie sequence
      cookie_sequence_++;
      // Generate new cookie by xor'ing initial cookie with sequence number.
      cookie = initial_cookie_ ^ cookie_sequence_;
    } else {
      // Client has already been notified. let's check if client has acknowledged it
      ZX_DEBUG_ASSERT(last_cookie_sent_ != 0);
      if (handler_.LatestAckedCookie() == last_cookie_sent_) {
        // Client has acknowledged cookie. Reset vsync tracking states
        number_of_vsyncs_sent_ = 0;
        acknowledge_request_sent_ = false;
        last_cookie_sent_ = 0;
      }
    }
  }

  if (number_of_vsyncs_sent_ >= kMaxVsyncMessages) {
    // We have reached/exceeded maximum allowed vsyncs without any acknowledgement. At this point,
    // start storing them
    zxlogf(TRACE, "Vsync not sent due to none acknowledgment.\n");
    ZX_DEBUG_ASSERT(cookie == 0);  // cookie should be zero!
    if (buffered_vsync_messages_.full()) {
      buffered_vsync_messages_.pop();  // discard
    }
    vsync_msg_t v = {
        .display_id = display_id,
        .timestamp = timestamp,
        .config_stamp = client_stamp,
    };
    buffered_vsync_messages_.push(v);
    return ZX_ERR_BAD_STATE;
  }

  auto cleanup = fit::defer([&]() {
    if (cookie) {
      cookie_sequence_--;
    }
    // Make sure status is not ZX_ERR_BAD_HANDLE, otherwise, depending on
    // policy setting channel write will crash
    ZX_DEBUG_ASSERT(event_sending_result.status() != ZX_ERR_BAD_HANDLE);
    if (event_sending_result.status() == ZX_ERR_NO_MEMORY) {
      total_oom_errors_++;
      // OOM errors are most likely not recoverable. Print the error message
      // once every kChannelErrorPrintFreq cycles
      if (chn_oom_print_freq_++ == 0) {
        zxlogf(ERROR, "Failed to send vsync event (OOM) (total occurrences: %lu)",
               total_oom_errors_);
      }
      if (chn_oom_print_freq_ >= kChannelOomPrintFreq) {
        chn_oom_print_freq_ = 0;
      }
    } else {
      zxlogf(WARNING, "Failed to send vsync event: %s",
             event_sending_result.FormatDescription().c_str());
    }
  });

  // Send buffered vsync events before sending the latest
  while (!buffered_vsync_messages_.empty()) {
    vsync_msg_t v = buffered_vsync_messages_.front();
    buffered_vsync_messages_.pop();
    event_sending_result = handler_.binding_state().SendEvents([&](auto&& endpoint) {
      return fidl::WireSendEvent(endpoint)->OnVsync(ToFidlDisplayId(v.display_id), v.timestamp,
                                                    ToFidlConfigStamp(v.config_stamp), 0);
    });
    if (!event_sending_result.ok()) {
      zxlogf(ERROR, "Failed to send all buffered vsync messages: %s\n",
             event_sending_result.FormatDescription().c_str());
      return event_sending_result.status();
    }
    number_of_vsyncs_sent_++;
  }

  // Send the latest vsync event
  event_sending_result = handler_.binding_state().SendEvents([&](auto&& endpoint) {
    return fidl::WireSendEvent(endpoint)->OnVsync(ToFidlDisplayId(display_id), timestamp,
                                                  ToFidlConfigStamp(client_stamp), cookie);
  });
  if (!event_sending_result.ok()) {
    return event_sending_result.status();
  }

  // Update vsync tracking states
  if (cookie) {
    acknowledge_request_sent_ = true;
    last_cookie_sent_ = cookie;
  }
  number_of_vsyncs_sent_++;
  cleanup.cancel();
  return ZX_OK;
}  // namespace display

void ClientProxy::OnClientDead() {
  // Copy over the on_client_dead function so we can call it after
  // freeing the class.
  fit::function<void()> on_client_dead = std::move(on_client_dead_);

  // This function deletes `this`. Be careful about not using class variables
  // it completes.
  controller_->OnClientDead(this);

  if (on_client_dead) {
    on_client_dead();
  }
}

void ClientProxy::UpdateConfigStampMapping(ConfigStampPair stamps) {
  ZX_DEBUG_ASSERT(pending_applied_config_stamps_.empty() ||
                  pending_applied_config_stamps_.back().controller_stamp < stamps.controller_stamp);
  pending_applied_config_stamps_.push_back({
      .controller_stamp = stamps.controller_stamp,
      .client_stamp = stamps.client_stamp,
  });
}

void ClientProxy::CloseTest() { handler_.TearDownTest(); }

void ClientProxy::CloseOnControllerLoop() {
  auto task = new async::Task();
  task->set_handler([client_handler = &handler_](async_dispatcher_t* dispatcher, async::Task* task,
                                                 zx_status_t status) {
    client_handler->TearDown();
    delete task;
  });
  if (task->Post(controller_->loop().dispatcher()) != ZX_OK) {
    // Tasks only fail to post if the looper is dead. That can happen if the controller is unbinding
    // and shutting down active clients, but if it does then it's safe to call Reset on this thread
    // anyway.
    delete task;
    handler_.TearDown();
  }
}

zx_status_t ClientProxy::Init(inspect::Node* parent_node,
                              fidl::ServerEnd<fuchsia_hardware_display::Coordinator> server_end) {
  node_ = parent_node->CreateChild(fbl::StringPrintf("client-%d", handler_.id()).c_str());
  node_.CreateBool("primary", !is_vc_, &static_properties_);
  is_owner_property_ = node_.CreateBool("is_owner", false);

  mtx_init(&task_mtx_, mtx_plain);
  auto seed = static_cast<uint32_t>(zx::clock::get_monotonic().get());
  initial_cookie_ = rand_r(&seed);
  auto result = handler_.Init(std::move(server_end));
  if (result.is_error()) {
    return result.error();
  }
  return ZX_OK;
}

ClientProxy::ClientProxy(Controller* controller, bool is_vc, uint32_t client_id,
                         fit::function<void()> on_client_dead)
    : controller_(controller),
      is_vc_(is_vc),
      handler_(controller_, this, is_vc_, client_id),
      on_client_dead_(std::move(on_client_dead)) {
  mtx_init(&mtx_, mtx_plain);
}

ClientProxy::ClientProxy(Controller* controller, bool is_vc, uint32_t client_id,
                         fidl::ServerEnd<fhd::Coordinator> server_end)
    : controller_(controller),
      is_vc_(is_vc),
      handler_(controller_, this, is_vc_, client_id, std::move(server_end)) {
  mtx_init(&mtx_, mtx_plain);
}

ClientProxy::~ClientProxy() {
  mtx_destroy(&mtx_);
  mtx_destroy(&task_mtx_);
}

}  // namespace display

// Banjo C macros and Fidl C++ enum member names collide. Some trickery is required.
namespace {

constexpr auto banjo_CLIENT_USE_PRIMARY = CLIENT_USE_PRIMARY;
constexpr auto banjo_CLIENT_MERGE_BASE = CLIENT_MERGE_BASE;
constexpr auto banjo_CLIENT_MERGE_SRC = CLIENT_MERGE_SRC;
constexpr auto banjo_CLIENT_FRAME_SCALE = CLIENT_FRAME_SCALE;
constexpr auto banjo_CLIENT_SRC_FRAME = CLIENT_SRC_FRAME;
constexpr auto banjo_CLIENT_TRANSFORM = CLIENT_TRANSFORM;
constexpr auto banjo_CLIENT_COLOR_CONVERSION = CLIENT_COLOR_CONVERSION;
constexpr auto banjo_CLIENT_ALPHA = CLIENT_ALPHA;
#undef CLIENT_USE_PRIMARY
#undef CLIENT_MERGE_BASE
#undef CLIENT_MERGE_SRC
#undef CLIENT_FRAME_SCALE
#undef CLIENT_SRC_FRAME
#undef CLIENT_TRANSFORM
#undef CLIENT_COLOR_CONVERSION
#undef CLIENT_ALPHA

static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientUsePrimary)) ==
                  banjo_CLIENT_USE_PRIMARY,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientMergeBase)) ==
                  banjo_CLIENT_MERGE_BASE,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientMergeSrc)) ==
                  banjo_CLIENT_MERGE_SRC,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientFrameScale)) ==
                  banjo_CLIENT_FRAME_SCALE,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientSrcFrame)) ==
                  banjo_CLIENT_SRC_FRAME,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientTransform)) ==
                  banjo_CLIENT_TRANSFORM,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientColorConversion)) ==
                  banjo_CLIENT_COLOR_CONVERSION,
              "Const mismatch");
static_assert((1 << static_cast<int>(fhd::wire::ClientCompositionOpcode::kClientAlpha)) ==
                  banjo_CLIENT_ALPHA,
              "Const mismatch");

}  // namespace
