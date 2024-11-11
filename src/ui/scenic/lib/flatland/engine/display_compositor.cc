// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/engine/display_compositor.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fidl/fuchsia.images2/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.sysmem/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/hlcpp_conversion.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/trace/event.h>
#include <zircon/status.h>

#include <cstdint>
#include <vector>

#include "fidl/fuchsia.hardware.display.types/cpp/common_types.h"
#include "lib/fidl/cpp/hlcpp_conversion.h"
#include "lib/fidl/cpp/wire/status.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/scenic/lib/allocation/id.h"
#include "src/ui/scenic/lib/display/util.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/utils/helpers.h"

namespace flatland {

namespace {

// Debugging color used to highlight images that have gone through the GPU rendering path.
const std::array<float, 4> kGpuRenderingDebugColor = {0.9f, 0.5f, 0.5f, 1.f};

const fuchsia_hardware_display::EventId kInvalidEventId = {
    fuchsia_hardware_display_types::kInvalidDispId};

// Returns an image type that describes the tiling format used for buffer with
// this pixel format. The values are display driver specific and not documented
// in the display coordinator FIDL API.
// TODO(https://fxbug.dev/42108519): Remove this when image type is removed from the display
// coordinator API.
uint32_t BufferCollectionPixelFormatToImageTilingType(
    fuchsia::images2::PixelFormatModifier pixel_format_modifier) {
  switch (pixel_format_modifier) {
    case fuchsia::images2::PixelFormatModifier::INTEL_I915_X_TILED:
      return 1;  // IMAGE_TILING_TYPE_X_TILED
    case fuchsia::images2::PixelFormatModifier::INTEL_I915_Y_TILED:
      return 2;  // IMAGE_TILING_TYPE_Y_LEGACY_TILED
    case fuchsia::images2::PixelFormatModifier::INTEL_I915_YF_TILED:
      return 3;  // IMAGE_TILING_TYPE_YF_TILED
    case fuchsia::images2::PixelFormatModifier::LINEAR:
    default:
      return fuchsia_hardware_display_types::kImageTilingTypeLinear;
  }
}

fuchsia_hardware_display_types::AlphaMode GetAlphaMode(
    const fuchsia_ui_composition::BlendMode& blend_mode) {
  fuchsia_hardware_display_types::AlphaMode alpha_mode;
  switch (blend_mode) {
    case fuchsia_ui_composition::BlendMode::kSrc:
      alpha_mode = fuchsia_hardware_display_types::AlphaMode::kDisable;
      break;
    case fuchsia_ui_composition::BlendMode::kSrcOver:
      alpha_mode = fuchsia_hardware_display_types::AlphaMode::kPremultiplied;
      break;
  }
  return alpha_mode;
}

// Creates a duplicate of |token| in |duplicate|.
// Returns an error string if it fails, otherwise std::nullopt.
std::optional<std::string> DuplicateToken(
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr& token,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr& duplicate) {
  fuchsia::sysmem2::BufferCollectionTokenDuplicateSyncRequest dup_sync_request;
  dup_sync_request.set_rights_attenuation_masks({ZX_RIGHT_SAME_RIGHTS});
  fuchsia::sysmem2::BufferCollectionToken_DuplicateSync_Result dup_sync_result;
  auto status = token->DuplicateSync(std::move(dup_sync_request), &dup_sync_result);
  if (status != ZX_OK) {
    return std::string("Could not duplicate token - status: ") + zx_status_get_string(status);
  }
  if (dup_sync_result.is_framework_err()) {
    return std::string("Could not duplicate token - framework_err");
  }
  FX_DCHECK(dup_sync_result.response().tokens().size() == 1);
  duplicate = dup_sync_result.response().mutable_tokens()->front().BindSync();
  return std::nullopt;
}

// Returns a prunable subtree of |token| with |num_new_tokens| children.
// Returns std::nullopt on failure.
std::optional<std::vector<fuchsia::sysmem2::BufferCollectionTokenSyncPtr>> CreatePrunableChildren(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr& token, const size_t num_new_tokens) {
  fuchsia::sysmem2::BufferCollectionTokenGroupSyncPtr token_group;
  fuchsia::sysmem2::BufferCollectionTokenCreateBufferCollectionTokenGroupRequest
      create_group_request;
  create_group_request.set_group_request(token_group.NewRequest());
  if (const auto status = token->CreateBufferCollectionTokenGroup(std::move(create_group_request));
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Could not create buffer collection token group: "
                   << zx_status_get_string(status);
    return std::nullopt;
  }

  // Create the requested children, then mark all children created and close out |token_group|.
  std::vector<zx_rights_t> children_request_rights(num_new_tokens, ZX_RIGHT_SAME_RIGHTS);
  fuchsia::sysmem2::BufferCollectionTokenGroupCreateChildrenSyncRequest create_children_request;
  create_children_request.set_rights_attenuation_masks(std::move(children_request_rights));
  fuchsia::sysmem2::BufferCollectionTokenGroup_CreateChildrenSync_Result create_children_result;
  {
    auto status = token_group->CreateChildrenSync(std::move(create_children_request),
                                                  &create_children_result);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "Could not create buffer collection token group children - status: "
                     << zx_status_get_string(status);
      return std::nullopt;
    }
    if (create_children_result.is_framework_err()) {
      FX_LOGS(ERROR) << "Could not create buffer collection token group children - framework_err: "
                     << fidl::ToUnderlying(create_children_result.framework_err());
      return std::nullopt;
    }
  }
  if (const auto status = token_group->AllChildrenPresent(); status != ZX_OK) {
    FX_LOGS(ERROR) << "Could not call AllChildrenPresent: " << zx_status_get_string(status);
    return std::nullopt;
  }
  if (const auto status = token_group->Release(); status != ZX_OK) {
    FX_LOGS(ERROR) << "Could not release token group: " << zx_status_get_string(status);
    return std::nullopt;
  }

  std::vector<fuchsia::sysmem2::BufferCollectionTokenSyncPtr> out_tokens;
  for (auto& new_token : *create_children_result.response().mutable_tokens()) {
    out_tokens.push_back(new_token.BindSync());
  }
  FX_DCHECK(out_tokens.size() == num_new_tokens);
  return out_tokens;
}

// Returns a BufferCollectionSyncPtr duplicate of |token| with empty constraints set.
// Since it has the same failure domain as |token|, it can be used to check the status of
// allocations made from that collection.
std::optional<fuchsia::sysmem2::BufferCollectionSyncPtr>
CreateDuplicateBufferCollectionPtrWithEmptyConstraints(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr& token) {
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr token_dup;
  if (auto error = DuplicateToken(token, token_dup)) {
    FX_LOGS(ERROR) << *error;
    return std::nullopt;
  }

  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(token_dup));
  bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
  sysmem_allocator->BindSharedCollection(std::move(bind_shared_request));

  if (const auto status = buffer_collection->SetConstraints(
          fuchsia::sysmem2::BufferCollectionSetConstraintsRequest{});
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Could not set constraints: " << zx_status_get_string(status);
    return std::nullopt;
  }

  return buffer_collection;
}

// Returns whether |metadata| describes a valid image.
bool IsValidBufferImage(const allocation::ImageMetadata& metadata) {
  if (metadata.identifier == 0) {
    FX_LOGS(ERROR) << "ImageMetadata identifier is invalid.";
    return false;
  }

  if (metadata.collection_id == allocation::kInvalidId) {
    FX_LOGS(ERROR) << "ImageMetadata collection ID is invalid.";
    return false;
  }

  if (metadata.width == 0 || metadata.height == 0) {
    FX_LOGS(ERROR) << "ImageMetadata has a null dimension: "
                   << "(" << metadata.width << ", " << metadata.height << ").";
    return false;
  }

  return true;
}

// Calls CheckBuffersAllocated |token| and returns whether the allocation succeeded.
bool CheckBuffersAllocated(fuchsia::sysmem2::BufferCollectionSyncPtr& token) {
  fuchsia::sysmem2::BufferCollection_CheckAllBuffersAllocated_Result check_allocated_result;
  const auto check_status = token->CheckAllBuffersAllocated(&check_allocated_result);
  return check_status == ZX_OK && check_allocated_result.is_response();
}

// Calls WaitForBuffersAllocated() on |token| and returns the pixel format of the allocation.
// |token| must have already checked that buffers are allocated.
// TODO(https://fxbug.dev/42150686): Delete after we don't need the pixel format anymore.
fuchsia::images2::PixelFormatModifier GetPixelFormatModifier(
    fuchsia::sysmem2::BufferCollectionSyncPtr& token) {
  fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
  const auto wait_status = token->WaitForAllBuffersAllocated(&wait_result);
  FX_DCHECK(wait_status == ZX_OK) << "WaitForBuffersAllocated failed - status: " << wait_status;
  FX_DCHECK(!wait_result.is_framework_err()) << "WaitForBuffersAllocated failed - framework_err: "
                                             << fidl::ToUnderlying(wait_result.framework_err());
  FX_DCHECK(!wait_result.is_err())
      << "WaitForBuffersAllocated failed - err: " << static_cast<uint32_t>(wait_result.err());
  return wait_result.response()
      .buffer_collection_info()
      .settings()
      .image_format_constraints()
      .pixel_format_modifier();
}

// Consumes |token| and if its allocation is compatible with the display returns its pixel format.
// Otherwise returns std::nullopt.
// TODO(https://fxbug.dev/42150686): Just return a bool after we don't need the pixel format
// anymore.
std::optional<fuchsia::images2::PixelFormatModifier> DetermineDisplaySupportFor(
    fuchsia::sysmem2::BufferCollectionSyncPtr token) {
  std::optional<fuchsia::images2::PixelFormatModifier> result = std::nullopt;

  const bool image_supports_display = CheckBuffersAllocated(token);
  if (image_supports_display) {
    result = GetPixelFormatModifier(token);
  }

  token->Release();
  return result;
}

}  // anonymous namespace

DisplayCompositor::DisplayCompositor(
    async_dispatcher_t* main_dispatcher,
    std::shared_ptr<fidl::SyncClient<fuchsia_hardware_display::Coordinator>> display_coordinator,
    const std::shared_ptr<Renderer>& renderer, fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator,
    const bool enable_display_composition, uint32_t max_display_layers,
    uint8_t visual_debugging_level)
    : display_coordinator_shared_ptr_(std::move(display_coordinator)),
      display_coordinator_(*display_coordinator_shared_ptr_),
      renderer_(renderer),
      release_fence_manager_(main_dispatcher),
      sysmem_allocator_(std::move(sysmem_allocator)),
      enable_display_composition_(enable_display_composition),
      max_display_layers_(max_display_layers),
      main_dispatcher_(main_dispatcher),
      visual_debugging_level_(visual_debugging_level) {
  FX_CHECK(main_dispatcher_);
  FX_DCHECK(renderer_);
  FX_DCHECK(sysmem_allocator_);
  FX_DCHECK(display_coordinator_shared_ptr_);
}

DisplayCompositor::~DisplayCompositor() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  // Destroy all of the display layers.
  DiscardConfig();
  for (const auto& [_, data] : display_engine_data_map_) {
    for (const fuchsia_hardware_display::LayerId& layer : data.layers) {
      fit::result<fidl::OneWayStatus> result = display_coordinator_->DestroyLayer(layer);
      if (result.is_error()) {
        FX_LOGS(ERROR) << "Failed to call FIDL DestroyLayer method: " << result.error_value();
      }
    }
    for (const auto& event_data : data.frame_event_datas) {
      fit::result<fidl::OneWayStatus> result =
          display_coordinator_->ReleaseEvent(event_data.wait_id);
      if (result.is_error()) {
        FX_LOGS(ERROR) << "Failed to call FIDL ReleaseEvent on wait event ("
                       << event_data.wait_id.value() << "): " << result.error_value();
      }
    }
  }

  // TODO(https://fxbug.dev/42063495): Release |render_targets| and |protected_render_targets|
  // collections and images.
}

bool DisplayCompositor::ImportBufferCollection(
    const allocation::GlobalBufferCollectionId collection_id,
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
    const BufferCollectionUsage usage, const std::optional<fuchsia::math::SizeU> size) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::ImportBufferCollection");
  FX_DCHECK(usage == BufferCollectionUsage::kClientImage)
      << "Expected default buffer collection usage";

  auto renderer_token = token.BindSync();

  // We want to achieve one of two outcomes:
  // 1. Allocate buffer that is compatible with both the renderer and the display
  // or, if that fails,
  // 2. Allocate a buffer that is only compatible with the renderer.
  // To do this we create two prunable children of the renderer token, one with display constraints
  // and one with no constraints. Only one of these children will be chosen during sysmem
  // negotiations.
  // Resulting tokens:
  // * renderer_token
  // . * token_group
  // . . * display_token (+ duplicate with no constraints to check allocation with, created below)
  // . . * Empty token
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr display_token;
  if (auto prunable_tokens =
          CreatePrunableChildren(sysmem_allocator, renderer_token, /*num_new_tokens*/ 2)) {
    // Display+Renderer should have higher priority than Renderer only.
    display_token = std::move(prunable_tokens->at(0));

    // We close the second token with setting any constraints. If this gets chosen during sysmem
    // negotiations then the allocated buffers are display-incompatible and we don't need to keep a
    // reference to them here.
    if (const auto status = prunable_tokens->at(1)->Release(); status != ZX_OK) {
      FX_LOGS(ERROR) << "Could not close token: " << zx_status_get_string(status);
    }
  } else {
    return false;
  }

  // Set renderer constraints.
  if (!renderer_->ImportBufferCollection(collection_id, sysmem_allocator, std::move(renderer_token),
                                         usage, size)) {
    FX_LOGS(ERROR) << "Renderer could not import buffer collection.";
    return false;
  }

  if (!enable_display_composition_) {
    // Forced fallback to using the renderer; don't attempt direct-to-display.
    // Close |display_token| without importing it to the display coordinator.
    if (const auto status = display_token->Release(); status != ZX_OK) {
      FX_LOGS(ERROR) << "Could not close token: " << zx_status_get_string(status);
    }
    return true;
  }

  // Create a BufferCollectionPtr from a duplicate of |display_token| with which to later check if
  // buffers allocated from the BufferCollection are display-compatible.
  auto collection_ptr =
      CreateDuplicateBufferCollectionPtrWithEmptyConstraints(sysmem_allocator, display_token);
  if (!collection_ptr.has_value()) {
    return false;
  }

  std::scoped_lock lock(lock_);
  {
    const auto [_, success] =
        display_buffer_collection_ptrs_.emplace(collection_id, std::move(*collection_ptr));
    FX_DCHECK(success);
  }

  // Import the buffer collection into the display coordinator, setting display constraints.
  fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> natural_display_token(
      std::move(display_token).Unbind().TakeChannel());
  return ImportBufferCollectionToDisplayCoordinator(
      collection_id, std::move(natural_display_token),
      fuchsia_hardware_display_types::ImageBufferUsage{
          {.tiling_type = fuchsia_hardware_display_types::kImageTilingTypeLinear}});
}

void DisplayCompositor::ReleaseBufferCollection(
    const allocation::GlobalBufferCollectionId collection_id, const BufferCollectionUsage usage) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::ReleaseBufferCollection");
  FX_DCHECK(usage == BufferCollectionUsage::kClientImage);

  renderer_->ReleaseBufferCollection(collection_id, usage);

  std::scoped_lock lock(lock_);
  FX_DCHECK(display_coordinator_.is_valid());
  const fuchsia_hardware_display::BufferCollectionId display_collection_id =
      scenic_impl::ToDisplayFidlBufferCollectionId(collection_id);
  const fit::result<fidl::OneWayStatus> result = display_coordinator_->ReleaseBufferCollection(
      {{.buffer_collection_id = display_collection_id}});
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to call FIDL ReleaseBufferCollection method: "
                   << result.error_value();
  }
  display_buffer_collection_ptrs_.erase(collection_id);
  buffer_collection_supports_display_.erase(collection_id);
}

fuchsia::sysmem2::BufferCollectionSyncPtr DisplayCompositor::TakeDisplayBufferCollectionPtr(
    const allocation::GlobalBufferCollectionId collection_id) {
  const auto token_it = display_buffer_collection_ptrs_.find(collection_id);
  FX_DCHECK(token_it != display_buffer_collection_ptrs_.end());
  auto token = std::move(token_it->second);
  display_buffer_collection_ptrs_.erase(token_it);
  return token;
}

fuchsia_hardware_display_types::ImageMetadata DisplayCompositor::CreateImageMetadata(
    const allocation::ImageMetadata& metadata) const {
  // TODO(https://fxbug.dev/42150686): Pixel format should be ignored when using sysmem. We do not
  // want to have to deal with this default image format. Work was in progress to address this, but
  // is currently stalled: see fxr/716543.
  FX_DCHECK(buffer_collection_pixel_format_modifier_.count(metadata.collection_id));
  const auto pixel_format_modifier =
      buffer_collection_pixel_format_modifier_.at(metadata.collection_id);
  return fuchsia_hardware_display_types::ImageMetadata{
      {.width = metadata.width,
       .height = metadata.height,
       .tiling_type = BufferCollectionPixelFormatToImageTilingType(pixel_format_modifier)}};
}

bool DisplayCompositor::ImportBufferImage(const allocation::ImageMetadata& metadata,
                                          const BufferCollectionUsage usage) {
  // Called from main thread or Flatland threads.
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::ImportBufferImage");

  if (!IsValidBufferImage(metadata)) {
    return false;
  }

  if (!renderer_->ImportBufferImage(metadata, usage)) {
    FX_LOGS(ERROR) << "Renderer could not import image.";
    return false;
  }

  std::scoped_lock lock(lock_);
  FX_DCHECK(display_coordinator_.is_valid());

  const allocation::GlobalBufferCollectionId collection_id = metadata.collection_id;
  const fuchsia_hardware_display::BufferCollectionId display_collection_id =
      scenic_impl::ToDisplayFidlBufferCollectionId(collection_id);
  const bool display_support_already_set =
      buffer_collection_supports_display_.find(collection_id) !=
      buffer_collection_supports_display_.end();

  // When display composition is disabled, the only images that should be imported by the display
  // are the framebuffers, and their display support is already set in AddDisplay() (instead of
  // below). For every other image with display composition off mode we can early exit.
  if (!enable_display_composition_ &&
      (!display_support_already_set || !buffer_collection_supports_display_[collection_id])) {
    buffer_collection_supports_display_[collection_id] = false;
    return true;
  }

  if (!display_support_already_set) {
    const auto pixel_format_modifier =
        DetermineDisplaySupportFor(TakeDisplayBufferCollectionPtr(collection_id));
    buffer_collection_supports_display_[collection_id] = pixel_format_modifier.has_value();
    if (pixel_format_modifier.has_value()) {
      buffer_collection_pixel_format_modifier_[collection_id] = pixel_format_modifier.value();
    }
  }

  if (!buffer_collection_supports_display_[collection_id]) {
    // When display isn't supported we fallback to using the renderer.
    return true;
  }

  const fuchsia_hardware_display_types::ImageMetadata image_metadata =
      CreateImageMetadata(metadata);
  const fuchsia_hardware_display::ImageId fidl_image_id =
      scenic_impl::ToDisplayFidlImageId(metadata.identifier);
  const fidl::Result import_image_result = display_coordinator_->ImportImage({{
      .image_metadata = image_metadata,
      .buffer_id = {{
          .buffer_collection_id = display_collection_id,
          .buffer_index = metadata.vmo_index,
      }},
      .image_id = fidl_image_id,
  }});
  if (import_image_result.is_error()) {
    FX_LOGS(ERROR) << "Display coordinator could not import the image: "
                   << import_image_result.error_value();
    return false;
  }

  display_imported_images_.insert(metadata.identifier);
  return true;
}

void DisplayCompositor::ReleaseBufferImage(const allocation::GlobalImageId image_id) {
  // Called from main thread or Flatland threads.
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::ReleaseBufferImage");
  FX_DCHECK(image_id != allocation::kInvalidImageId);

  renderer_->ReleaseBufferImage(image_id);

  const fuchsia_hardware_display::ImageId fidl_image_id =
      scenic_impl::ToDisplayFidlImageId(image_id);
  std::scoped_lock lock(lock_);

  if (display_imported_images_.erase(image_id) == 1) {
    FX_DCHECK(display_coordinator_.is_valid());

    fit::result<fidl::OneWayStatus> result =
        display_coordinator_->ReleaseImage({{.image_id = fidl_image_id}});
    if (result.is_error()) {
      FX_LOGS(ERROR) << "Failed to call FIDL ReleaseImage method: " << result.error_value();
    }
  }
}

fuchsia_hardware_display::LayerId DisplayCompositor::CreateDisplayLayer() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(display_coordinator_.is_valid());

  const fidl::Result create_layer_result = display_coordinator_->CreateLayer();
  if (create_layer_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to create layer, " << create_layer_result.error_value();
    return {{.value = fuchsia_hardware_display_types::kInvalidDispId}};
  }
  return create_layer_result.value().layer_id();
}

void DisplayCompositor::SetDisplayLayers(
    const fuchsia_hardware_display_types::DisplayId display_id,
    const std::vector<fuchsia_hardware_display::LayerId>& layers) {
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::SetDisplayLayers");
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(display_coordinator_.is_valid());

  // Set all of the layers for each of the images on the display.
  const fit::result<fidl::OneWayStatus> set_display_layers_result =
      display_coordinator_->SetDisplayLayers({{
          .display_id = display_id,
          .layer_ids = layers,
      }});
  FX_DCHECK(set_display_layers_result.is_ok())
      << "Failed to call FIDL SetDisplayLayers method: " << set_display_layers_result.error_value();
}

bool DisplayCompositor::SetRenderDataOnDisplay(const RenderData& data) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  // Every rectangle should have an associated image.
  const uint32_t num_images = static_cast<uint32_t>(data.images.size());

  // Since we map 1 image to 1 layer, if there are more images than layers available for
  // the given display, then they cannot be directly composited to the display in hardware.
  const std::vector<fuchsia_hardware_display::LayerId>& layers =
      display_engine_data_map_.at(data.display_id.value()).layers;
  if (layers.size() < num_images) {
    return false;
  }

  // We only set as many layers as needed for the images we have.
  SetDisplayLayers(data.display_id, std::vector<fuchsia_hardware_display::LayerId>(
                                        layers.begin(), layers.begin() + num_images));

  for (uint32_t i = 0; i < num_images; i++) {
    const allocation::GlobalImageId image_id = data.images[i].identifier;
    if (image_id != allocation::kInvalidImageId) {
      if (buffer_collection_supports_display_[data.images[i].collection_id]) {
        ApplyLayerImage(layers[i], data.rectangles[i], data.images[i],
                        /*wait_id*/ kInvalidEventId,
                        /*signal_id*/ kInvalidEventId);
      } else {
        return false;
      }
    } else {
      // TODO(https://fxbug.dev/42056054): Not all display hardware is able to handle color layers
      // with specific sizes, which is required for doing solid-fill rects on the display path. If
      // we encounter one of those rects here -- unless it is the backmost layer and fullscreen
      // -- then we abort.
      const auto& rect = data.rectangles[i];
      const glm::uvec2& display_size = display_info_map_[data.display_id.value()].dimensions;
      if (i == 0 && rect.origin.x == 0 && rect.origin.y == 0 &&
          rect.extent.x == static_cast<float>(display_size.x) &&
          rect.extent.y == static_cast<float>(display_size.y)) {
        ApplyLayerColor(layers[i], rect, data.images[i]);
      } else {
        return false;
      }
    }
  }

  return true;
}

void DisplayCompositor::ApplyLayerColor(const fuchsia_hardware_display::LayerId& layer_id,
                                        const ImageRect& rectangle,
                                        const allocation::ImageMetadata& image) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(display_coordinator_.is_valid());

  // We have to convert the image_metadata's multiply color, which is an array of normalized
  // floating point values, to an unnormalized array of uint8_ts in the range 0-255.
  const std::array<uint8_t, 8> color_bytes = {static_cast<uint8_t>(255 * image.multiply_color[0]),
                                              static_cast<uint8_t>(255 * image.multiply_color[1]),
                                              static_cast<uint8_t>(255 * image.multiply_color[2]),
                                              static_cast<uint8_t>(255 * image.multiply_color[3]),
                                              0,
                                              0,
                                              0,
                                              0};

  const fit::result<fidl::OneWayStatus> set_layer_color_result =
      display_coordinator_->SetLayerColorConfig({{
          .layer_id = layer_id,
          .color = fuchsia_hardware_display_types::Color(fuchsia_images2::PixelFormat::kB8G8R8A8,
                                                         color_bytes),
      }});
  FX_DCHECK(set_layer_color_result.is_ok())
      << "Failed to call FIDL SetLayerColorConfig method: " << set_layer_color_result.error_value();

// TODO(https://fxbug.dev/42056054): Currently, not all display hardware supports the ability to
// set either the position or the alpha on a color layer, as color layers are not primary
// layers. There exist hardware that require a color layer to be the backmost layer and to be
// the size of the entire display. This means that for the time being, we must rely on GPU
// composition for solid color rects.
//
// There is the option of assigning a 1x1 image with the desired color to a standard image layer,
// as a way of mimicking color layers (and this is what is done in the GPU path as well) --
// however, not all hardware supports images with sizes that differ from the destination size of
// the rect. So implementing that solution on the display path as well is problematic.
#if 0

  const auto [src, dst] = DisplaySrcDstFrames::New(rectangle, image);

  const fuchsia_hardware_display_types::CoordinateTransformation transform =
      GetDisplayTransformFromOrientationAndFlip(rectangle.orientation, image.flip);

  const fit::result set_layer_position_result =
      display_coordinator_->SetLayerPrimaryPosition({{
        .layer_id = layer_id,
        .transform = transform,
        .src_frame = src,
        .dest_frame = dst,
      }});
  FX_DCHECK(set_layer_position_result.is_ok()) << "Failed to call FIDL SetLayerPrimaryPosition method: "
                                               << set_layer_position_result.error_value();

  const fuchsia_hardware_display_types::AlphaMode alpha_mode = GetAlphaMode(image.blend_mode);
  const fit::result set_layer_alpha_result =
      display_coordinator_->SetLayerPrimaryAlpha({{
        .layer_id = layer_id,
        .mode = alpha_mode,
        .val = image.multiply_color[3],
      }});
  FX_DCHECK(set_layer_alpha_result.is_ok()) << "Failed to call FIDL SetLayerPrimaryAlpha method: "
                                            << set_layer_alpha_result.error_value();
#endif
}

void DisplayCompositor::ApplyLayerImage(const fuchsia_hardware_display::LayerId& layer_id,
                                        const ImageRect& rectangle,
                                        const allocation::ImageMetadata& image,
                                        const scenic_impl::DisplayEventId& wait_id,
                                        const scenic_impl::DisplayEventId& signal_id) {
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::ApplyLayerImage");
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(display_coordinator_.is_valid());

  const auto [src, dst] = DisplaySrcDstFrames::New(rectangle, image);
  FX_DCHECK(src.width() && src.height()) << "Source frame cannot be empty.";
  FX_DCHECK(dst.width() && dst.height()) << "Destination frame cannot be empty.";
  const fuchsia_hardware_display_types::CoordinateTransformation transform =
      GetDisplayTransformFromOrientationAndFlip(rectangle.orientation, image.flip);
  const fuchsia_hardware_display_types::AlphaMode alpha_mode = GetAlphaMode(image.blend_mode);

  const fuchsia_hardware_display_types::ImageMetadata image_metadata = CreateImageMetadata(image);
  const fit::result<fidl::OneWayStatus> set_layer_primary_config_result =
      display_coordinator_->SetLayerPrimaryConfig({{
          .layer_id = layer_id,
          .image_metadata = image_metadata,
      }});
  FX_DCHECK(set_layer_primary_config_result.is_ok())
      << "Failed to call FIDL SetLayerPrimaryConfig method: "
      << set_layer_primary_config_result.error_value();

  const fit::result<fidl::OneWayStatus> set_layer_primary_position_result =
      display_coordinator_->SetLayerPrimaryPosition({{
          .layer_id = layer_id,
          .image_source_transformation = transform,
          .image_source = src,
          .display_destination = dst,
      }});

  FX_DCHECK(set_layer_primary_position_result.is_ok())
      << "Failed to call FIDL SetLayerPrimaryPosition method: "
      << set_layer_primary_position_result.error_value();

  const fit::result<fidl::OneWayStatus> set_layer_primary_alpha_result =
      display_coordinator_->SetLayerPrimaryAlpha({{
          .layer_id = layer_id,
          .mode = alpha_mode,
          .val = image.multiply_color[3],
      }});
  FX_DCHECK(set_layer_primary_alpha_result.is_ok())
      << "Failed to call FIDL SetLayerPrimaryAlpha method: "
      << set_layer_primary_alpha_result.error_value();

  // Set the imported image on the layer.
  const fuchsia_hardware_display::ImageId image_id =
      scenic_impl::ToDisplayFidlImageId(image.identifier);
  const fit::result<fidl::OneWayStatus> set_layer_image_result =
      display_coordinator_->SetLayerImage({{
          .layer_id = layer_id,
          .image_id = image_id,
          .wait_event_id = wait_id,
          .signal_event_id = signal_id,
      }});
  FX_DCHECK(set_layer_image_result.is_ok())
      << "Failed to call FIDL SetLayerImage method: " << set_layer_image_result.error_value();
}

bool DisplayCompositor::CheckConfig() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(display_coordinator_.is_valid());

  TRACE_DURATION("gfx", "flatland::DisplayCompositor::CheckConfig");
  const fidl::Result check_config_result = display_coordinator_->CheckConfig({{
      .discard = false,
  }});
  FX_DCHECK(check_config_result.is_ok())
      << "Failed to call FIDL CheckConfig method: " << check_config_result.error_value();
  return check_config_result.value().res() == fuchsia_hardware_display_types::ConfigResult::kOk;
}

void DisplayCompositor::DiscardConfig() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(display_coordinator_.is_valid());

  TRACE_DURATION("gfx", "flatland::DisplayCompositor::DiscardConfig");
  const fidl::Result check_config_result = display_coordinator_->CheckConfig({{
      .discard = true,
  }});
  FX_DCHECK(check_config_result.is_ok())
      << "Failed to call FIDL CheckConfig method: " << check_config_result.error_value();
}

fuchsia_hardware_display_types::ConfigStamp DisplayCompositor::ApplyConfig() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(display_coordinator_.is_valid());

  TRACE_DURATION("gfx", "flatland::DisplayCompositor::ApplyConfig");
  {
    const fit::result<fidl::OneWayStatus> result = display_coordinator_->ApplyConfig();
    FX_DCHECK(result.is_ok()) << "Failed to call FIDL ApplyConfig method: " << result.error_value();
  }

  {
    const fidl::Result result = display_coordinator_->GetLatestAppliedConfigStamp();
    FX_DCHECK(result.is_ok()) << "Failed to call FIDL GetLatestAppliedConfigStamp method: "
                              << result.error_value();
    return result.value().stamp();
  }
}

bool DisplayCompositor::PerformGpuComposition(const uint64_t frame_number,
                                              const zx::time presentation_time,
                                              const std::vector<RenderData>& render_data_list,
                                              std::vector<zx::event> release_fences,
                                              scheduling::FramePresentedCallback callback) {
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::PerformGpuComposition");
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  // Create an event that will be signaled when the final display's content has finished
  // rendering; it will be passed into |release_fence_manager_.OnGpuCompositedFrame()|.  If there
  // are multiple displays which require GPU-composited content, we pass this event to be signaled
  // when the final display's content has finished rendering (thus guaranteeing that all previous
  // content has also finished rendering).
  // TODO(https://fxbug.dev/42157678): we might want to reuse events, instead of creating a new one
  // every frame.
  zx::event render_finished_fence = utils::CreateEvent();

  for (size_t i = 0; i < render_data_list.size(); ++i) {
    const bool is_final_display = i == (render_data_list.size() - 1);
    const auto& render_data = render_data_list[i];
    const auto display_engine_data_it =
        display_engine_data_map_.find(render_data.display_id.value());
    FX_DCHECK(display_engine_data_it != display_engine_data_map_.end());
    auto& display_engine_data = display_engine_data_it->second;

    // Clear any past CC state here, before applying GPU CC.
    if (cc_state_machine_.GpuRequiresDisplayClearing()) {
      TRACE_DURATION("gfx", "flatland::DisplayCompositor::PerformGpuComposition[cc]");
      const fit::result<fidl::OneWayStatus> set_display_color_conversion_result =
          display_coordinator_->SetDisplayColorConversion(
              {{.display_id = render_data.display_id,
                .preoffsets = kDefaultColorConversionOffsets,
                .coefficients = kDefaultColorConversionCoefficients,
                .postoffsets = kDefaultColorConversionOffsets}});
      FX_CHECK(set_display_color_conversion_result.is_ok())
          << "Could not apply hardware color conversion: "
          << set_display_color_conversion_result.error_value();
      cc_state_machine_.DisplayCleared();
    }

    if (display_engine_data.vmo_count == 0) {
      FX_LOGS(WARNING) << "No VMOs were created when creating display "
                       << render_data.display_id.value() << ".";
      return false;
    }
    const uint32_t curr_vmo = display_engine_data.curr_vmo;
    display_engine_data.curr_vmo =
        (display_engine_data.curr_vmo + 1) % display_engine_data.vmo_count;
    const auto& render_targets = renderer_->RequiresRenderInProtected(render_data.images)
                                     ? display_engine_data.protected_render_targets
                                     : display_engine_data.render_targets;
    FX_DCHECK(curr_vmo < render_targets.size()) << curr_vmo << "/" << render_targets.size();
    FX_DCHECK(curr_vmo < display_engine_data.frame_event_datas.size())
        << curr_vmo << "/" << display_engine_data.frame_event_datas.size();
    const auto& render_target = render_targets[curr_vmo];

    // Reset the event data.
    auto& event_data = display_engine_data.frame_event_datas[curr_vmo];
    event_data.wait_event.signal(ZX_EVENT_SIGNALED, 0);

    // Apply the debugging color to the images.
    auto images = render_data.images;
    const uint8_t VISUAL_DEBUGGING_LEVEL_INFO_PLATFORM = 2;
    if (visual_debugging_level_ >= VISUAL_DEBUGGING_LEVEL_INFO_PLATFORM) {
      for (auto& image : images) {
        image.multiply_color[0] *= kGpuRenderingDebugColor[0];
        image.multiply_color[1] *= kGpuRenderingDebugColor[1];
        image.multiply_color[2] *= kGpuRenderingDebugColor[2];
        image.multiply_color[3] *= kGpuRenderingDebugColor[3];
      }
    }

    const auto apply_cc = (cc_state_machine_.GetDataToApply() != std::nullopt);
    std::vector<zx::event> render_fences;
    render_fences.push_back(std::move(event_data.wait_event));
    // Only add render_finished_fence if we're rendering the final display's framebuffer.
    if (is_final_display) {
      render_fences.push_back(std::move(render_finished_fence));
      renderer_->Render(render_target, render_data.rectangles, images, render_fences, apply_cc);
      // Retrieve fence.
      render_finished_fence = std::move(render_fences.back());
    } else {
      renderer_->Render(render_target, render_data.rectangles, images, render_fences, apply_cc);
    }

    // Retrieve fence.
    event_data.wait_event = std::move(render_fences[0]);

    const fuchsia_hardware_display::LayerId layer = display_engine_data.layers[0];
    SetDisplayLayers(render_data.display_id, {layer});
    ApplyLayerImage(layer, {glm::vec2(0), glm::vec2(render_target.width, render_target.height)},
                    render_target, event_data.wait_id, /*signal_id*/ kInvalidEventId);

    // We are being opportunistic and skipping the costly CheckConfig() call at this stage, because
    // we know that gpu composited layers work and there is no fallback case beyond this. See
    // https://fxbug.dev/42165041 for more details.
#ifndef NDEBUG
    if (!CheckConfig()) {
      FX_LOGS(ERROR) << "Both display hardware composition and GPU rendering have failed.";
      return false;
    }
#endif
  }

  // See ReleaseFenceManager comments for details.
  FX_DCHECK(render_finished_fence);
  release_fence_manager_.OnGpuCompositedFrame(frame_number, std::move(render_finished_fence),
                                              std::move(release_fences), std::move(callback));
  return true;
}

DisplayCompositor::RenderFrameResult DisplayCompositor::RenderFrame(
    const uint64_t frame_number, const zx::time presentation_time,
    const std::vector<RenderData>& render_data_list, std::vector<zx::event> release_fences,
    scheduling::FramePresentedCallback callback, RenderFrameTestArgs test_args) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "flatland::DisplayCompositor::RenderFrame");
  std::scoped_lock lock(lock_);

  // Determine whether we need to fall back to GPU composition. Avoid calling CheckConfig() if we
  // don't need to, because this requires a round-trip to the display coordinator.
  // Note: TryDirectToDisplay() failing indicates hardware failure to do display composition.
  const bool fallback_to_gpu_composition = !enable_display_composition_ ||
                                           test_args.force_gpu_composition ||
                                           !TryDirectToDisplay(render_data_list) || !CheckConfig();

  if (fallback_to_gpu_composition) {
    // Discard only if we have attempted to TryDirectToDisplay() and have an unapplied config.
    // DiscardConfig call is costly and we should avoid calling when it isn't necessary.
    if (enable_display_composition_) {
      DiscardConfig();
    }

    if (!PerformGpuComposition(frame_number, presentation_time, render_data_list,
                               std::move(release_fences), std::move(callback))) {
      return RenderFrameResult::kFailure;
    }
  } else {
    // CC was successfully applied to the config so we update the state machine.
    cc_state_machine_.SetApplyConfigSucceeded();

    // See ReleaseFenceManager comments for details.
    release_fence_manager_.OnDirectScanoutFrame(frame_number, std::move(release_fences),
                                                std::move(callback));
  }

  const fuchsia_hardware_display_types::ConfigStamp config_stamp = ApplyConfig();
  pending_apply_configs_.push_back({.config_stamp = config_stamp, .frame_number = frame_number});

  return fallback_to_gpu_composition ? RenderFrameResult::kGpuComposition
                                     : RenderFrameResult::kDirectToDisplay;
}

bool DisplayCompositor::TryDirectToDisplay(const std::vector<RenderData>& render_data_list) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FX_DCHECK(enable_display_composition_);

  // TODO(https://fxbug.dev/377979329): re-enable direct-to-display once we have relaxed the display
  // coordinator's restrictions on image reuse.
  return false;

  for (const auto& data : render_data_list) {
    if (!SetRenderDataOnDisplay(data)) {
      // TODO(https://fxbug.dev/42157429): just because setting the data on one display fails (e.g.
      // due to too many layers), that doesn't mean that all displays need to use GPU-composition.
      // Some day we might want to use GPU-composition for some client images, and direct-scanout
      // for others.
      return false;
    }

    // Check the state machine to see if there's any CC data to apply.
    if (const auto cc_data = cc_state_machine_.GetDataToApply()) {
      // Apply direct-to-display color conversion here.
      const fit::result<fidl::OneWayStatus> set_display_color_conversion_result =
          display_coordinator_->SetDisplayColorConversion({{
              .display_id = data.display_id,
              .preoffsets = (*cc_data).preoffsets,
              .coefficients = (*cc_data).coefficients,
              .postoffsets = (*cc_data).postoffsets,
          }});
      FX_CHECK(set_display_color_conversion_result.is_ok())
          << "Could not apply hardware color conversion: "
          << set_display_color_conversion_result.error_value();
    }
  }

  return true;
}

void DisplayCompositor::OnVsync(zx::time timestamp,
                                fuchsia_hardware_display_types::ConfigStamp applied_config_stamp) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "Flatland::DisplayCompositor::OnVsync");

  // We might receive multiple OnVsync() callbacks with the same |applied_config_stamp| if the scene
  // doesn't change. Early exit for these cases.
  if (last_presented_config_stamp_.has_value() &&
      applied_config_stamp == last_presented_config_stamp_.value()) {
    return;
  }

  // Verify that the configuration from Vsync is in the [pending_apply_configs_] queue.
  const auto vsync_frame_it =
      std::find_if(pending_apply_configs_.begin(), pending_apply_configs_.end(),
                   [applied_config_stamp](const ApplyConfigInfo& info) {
                     return info.config_stamp == applied_config_stamp;
                   });

  // It is possible that the config stamp doesn't match any config applied by this DisplayCompositor
  // instance. i.e. it could be from another client. Thus we just ignore these events.
  if (vsync_frame_it == pending_apply_configs_.end()) {
    FX_LOGS(INFO) << "The config stamp <" << applied_config_stamp.value() << "> was not generated "
                  << "by current DisplayCompositor. Vsync event skipped.";
    return;
  }

  // Handle the presented ApplyConfig() call, as well as the skipped ones.
  auto it = pending_apply_configs_.begin();
  auto end = std::next(vsync_frame_it);
  while (it != end) {
    release_fence_manager_.OnVsync(it->frame_number, timestamp);
    it = pending_apply_configs_.erase(it);
  }
  last_presented_config_stamp_ = applied_config_stamp;
}

DisplayCompositor::FrameEventData DisplayCompositor::NewFrameEventData() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  FrameEventData result;
  {  // The DC waits on this to be signaled by the renderer.
    const auto status = zx::event::create(0, &result.wait_event);
    FX_DCHECK(status == ZX_OK);
  }
  result.wait_id = scenic_impl::ImportEvent(display_coordinator_, result.wait_event);
  FX_DCHECK(result.wait_id.value() != fuchsia_hardware_display_types::kInvalidDispId);
  return result;
}

void DisplayCompositor::AddDisplay(scenic_impl::display::Display* display, const DisplayInfo info,
                                   const uint32_t num_render_targets,
                                   fuchsia::sysmem2::BufferCollectionInfo* out_collection_info) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());

  // Grab the best pixel format that the renderer prefers given the list of available formats on
  // the display.
  FX_DCHECK(!info.formats.empty());
  const auto pixel_format = renderer_->ChoosePreferredRenderTargetFormat(info.formats);

  const fuchsia::math::SizeU size = {/*width*/ info.dimensions.x, /*height*/ info.dimensions.y};

  const fuchsia_hardware_display_types::DisplayId display_id = display->display_id();
  FX_DCHECK(display_engine_data_map_.find(display_id.value()) == display_engine_data_map_.end())
      << "DisplayCompositor::AddDisplay(): display already exists: " << display_id.value();

  display_info_map_[display_id.value()] = std::move(info);
  DisplayEngineData& display_engine_data = display_engine_data_map_[display_id.value()];

  {
    std::scoped_lock lock(lock_);
    // When we add in a new display, we create a couple of layers for that display upfront to be
    // used when we directly composite render data in hardware via the display coordinator.
    // TODO(https://fxbug.dev/42157936): per-display layer lists are probably a bad idea; this
    // approach doesn't reflect the constraints of the underlying display hardware.
    for (uint32_t i = 0; i < max_display_layers_; i++) {
      display_engine_data.layers.push_back(CreateDisplayLayer());
    }
  }

  // Add vsync callback on display. Note that this will overwrite the existing callback on
  // |display| and other clients won't receive any, i.e. gfx.
  display->SetVsyncCallback(
      [weak_ref = weak_from_this()](
          zx::time timestamp, fuchsia_hardware_display_types::ConfigStamp applied_config_stamp) {
        if (auto ref = weak_ref.lock())
          ref->OnVsync(timestamp, applied_config_stamp);
      });

  // Exit early if there are no vmos to create.
  if (num_render_targets == 0) {
    return;
  }

  // If we are creating vmos, we need a non-null buffer collection pointer to return back
  // to the caller.
  FX_DCHECK(out_collection_info);
  auto pixel_format_clone = pixel_format;
  display_engine_data.render_targets = AllocateDisplayRenderTargets(
      /*use_protected_memory=*/false, num_render_targets, size,
      fidl::NaturalToHLCPP(pixel_format_clone), out_collection_info);

  {
    std::scoped_lock lock(lock_);
    for (uint32_t i = 0; i < num_render_targets; i++) {
      display_engine_data.frame_event_datas.push_back(NewFrameEventData());
    }
  }
  display_engine_data.vmo_count = num_render_targets;
  display_engine_data.curr_vmo = 0;

  // Create another set of tokens and allocate a protected render target. Protected memory buffer
  // pool is usually limited, so it is better for Scenic to preallocate to avoid being blocked by
  // running out of protected memory.
  if (renderer_->SupportsRenderInProtected()) {
    display_engine_data.protected_render_targets = AllocateDisplayRenderTargets(
        /*use_protected_memory=*/true, num_render_targets, size,
        fidl::NaturalToHLCPP(pixel_format_clone), nullptr);
  }
}

void DisplayCompositor::SetColorConversionValues(const std::array<float, 9>& coefficients,
                                                 const std::array<float, 3>& preoffsets,
                                                 const std::array<float, 3>& postoffsets) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  cc_state_machine_.SetData(
      {.coefficients = coefficients, .preoffsets = preoffsets, .postoffsets = postoffsets});

  renderer_->SetColorConversionValues(coefficients, preoffsets, postoffsets);
}

bool DisplayCompositor::SetMinimumRgb(const uint8_t minimum_rgb) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  std::scoped_lock lock(lock_);
  FX_DCHECK(display_coordinator_.is_valid());

  const fidl::Result result = display_coordinator_->SetMinimumRgb({{
      .minimum_rgb = minimum_rgb,
  }});
  if (result.is_error()) {
    FX_LOGS(ERROR) << "FlatlandDisplayCompositor SetMinimumRGB failed: " << result.error_value();
    return false;
  }
  return true;
}

std::vector<allocation::ImageMetadata> DisplayCompositor::AllocateDisplayRenderTargets(
    const bool use_protected_memory, const uint32_t num_render_targets,
    const fuchsia::math::SizeU& size, const fuchsia::images2::PixelFormat pixel_format,
    fuchsia::sysmem2::BufferCollectionInfo* out_collection_info) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  // Create the buffer collection token to be used for frame buffers.
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr compositor_token;
  {
    fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
    allocate_shared_request.set_token_request(compositor_token.NewRequest());
    const auto status =
        sysmem_allocator_->AllocateSharedCollection(std::move(allocate_shared_request));
    FX_DCHECK(status == ZX_OK) << "status: " << zx_status_get_string(status);
  }

  // Duplicate the token for the display and for the renderer.
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr renderer_token;
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr display_token;
  {
    fuchsia::sysmem2::BufferCollectionTokenDuplicateSyncRequest dup_sync_request;
    dup_sync_request.set_rights_attenuation_masks({ZX_RIGHT_SAME_RIGHTS, ZX_RIGHT_SAME_RIGHTS});
    fuchsia::sysmem2::BufferCollectionToken_DuplicateSync_Result dup_sync_result;
    const auto status =
        compositor_token->DuplicateSync(std::move(dup_sync_request), &dup_sync_result);
    FX_DCHECK(status == ZX_OK) << "status: " << zx_status_get_string(status);
    FX_DCHECK(!dup_sync_result.is_framework_err())
        << "framework_err: " << fidl::ToUnderlying(dup_sync_result.framework_err());
    FX_DCHECK(dup_sync_result.is_response());
    auto dup_tokens = std::move(*dup_sync_result.response().mutable_tokens());
    FX_DCHECK(dup_tokens.size() == 2);
    renderer_token = dup_tokens.at(0).BindSync();
    display_token = dup_tokens.at(1).BindSync();

    constexpr size_t kMaxSysmem1DebugNameLength = 64;

    auto set_token_debug_name = [](fuchsia::sysmem2::BufferCollectionTokenSyncPtr& token,
                                   const char* token_name) {
      std::stringstream name_stream;
      name_stream << "AllocateDisplayRenderTargets " << token_name << " "
                  << fsl::GetCurrentProcessName();
      std::string token_client_name = name_stream.str();
      if (token_client_name.size() > kMaxSysmem1DebugNameLength) {
        token_client_name.resize(kMaxSysmem1DebugNameLength);
      }
      // set debug info for renderer_token in case it fails unexpectedly or similar
      fuchsia::sysmem2::NodeSetDebugClientInfoRequest set_debug_request;
      set_debug_request.set_name(std::move(token_client_name));
      set_debug_request.set_id(fsl::GetCurrentProcessKoid());
      auto set_info_status = token->SetDebugClientInfo(std::move(set_debug_request));
      FX_DCHECK(set_info_status == ZX_OK)
          << "set_info_status: " << zx_status_get_string(set_info_status);
    };

    set_token_debug_name(renderer_token, "renderer_token");
    set_token_debug_name(display_token, "display_token");

    // The compositor_token inherited it's debug info from sysmem_allocator_, so is still set to
    // "scenic flatland::DisplayCompositor" at this point, which is fine; just need to be able to
    // tell which token is potentially failing below - at this point each token (compositor_token,
    // renderer_token, display_token) has distinguishable debug info.
  }

  // Set renderer constraints.
  const auto collection_id = allocation::GenerateUniqueBufferCollectionId();
  {
    const auto result = renderer_->ImportBufferCollection(
        collection_id, sysmem_allocator_.get(), std::move(renderer_token),
        BufferCollectionUsage::kRenderTarget, std::optional<fuchsia::math::SizeU>(size));
    FX_DCHECK(result);
  }

  {  // Set display constraints.
    std::scoped_lock lock(lock_);
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> natural_display_token(
        std::move(display_token).Unbind().TakeChannel());
    const auto result = ImportBufferCollectionToDisplayCoordinator(
        collection_id, std::move(natural_display_token),
        fuchsia_hardware_display_types::ImageBufferUsage{
            {.tiling_type = fuchsia_hardware_display_types::kImageTilingTypeLinear}});
    FX_DCHECK(result);
  }

// Set local constraints.
#ifdef CPU_ACCESSIBLE_VMO
  const bool make_cpu_accessible = true;
#else
  const bool make_cpu_accessible = false;
#endif

  fuchsia::sysmem2::BufferCollectionSyncPtr collection_ptr;
  if (make_cpu_accessible && !use_protected_memory) {
    auto [buffer_usage, memory_constraints] = GetUsageAndMemoryConstraintsForCpuWriteOften();
    collection_ptr = CreateBufferCollectionSyncPtrAndSetConstraints(
        sysmem_allocator_.get(), std::move(compositor_token), num_render_targets, size.width,
        size.height, std::move(buffer_usage), pixel_format, std::move(memory_constraints));
  } else {
    fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
    auto& constraints = *set_constraints_request.mutable_constraints();
    constraints.set_min_buffer_count_for_camping(num_render_targets);
    constraints.mutable_usage()->set_none(fuchsia::sysmem2::NONE_USAGE);
    if (use_protected_memory) {
      auto& bmc = *constraints.mutable_buffer_memory_constraints();
      bmc.set_secure_required(true);
      bmc.set_inaccessible_domain_supported(true);
      bmc.set_cpu_domain_supported(false);
      bmc.set_ram_domain_supported(false);
    }

    fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.set_token(std::move(compositor_token));
    bind_shared_request.set_buffer_collection_request(collection_ptr.NewRequest());
    sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request));

    fuchsia::sysmem2::NodeSetNameRequest set_name_request;
    set_name_request.set_priority(10u);
    set_name_request.set_name(use_protected_memory
                                  ? "FlatlandDisplayCompositorProtectedRenderTarget"
                                  : "FlatlandDisplayCompositorRenderTarget");
    collection_ptr->SetName(std::move(set_name_request));

    const auto status = collection_ptr->SetConstraints(std::move(set_constraints_request));
    FX_DCHECK(status == ZX_OK) << "status: " << zx_status_get_string(status);
  }

  // Wait for buffers allocated so it can populate its information struct with the vmo data.
  fuchsia::sysmem2::BufferCollectionInfo collection_info;
  {
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    const auto status = collection_ptr->WaitForAllBuffersAllocated(&wait_result);
    FX_DCHECK(status == ZX_OK) << "status: " << zx_status_get_string(status);
    FX_DCHECK(!wait_result.is_framework_err())
        << "framework_err: " << fidl::ToUnderlying(wait_result.framework_err());
    FX_DCHECK(!wait_result.is_err()) << "err: " << static_cast<uint32_t>(wait_result.err());
    collection_info = std::move(*wait_result.response().mutable_buffer_collection_info());
  }

  {
    const auto status = collection_ptr->Release();
    FX_DCHECK(status == ZX_OK) << "status: " << zx_status_get_string(status);
  }

  // We know that this collection is supported by display because we collected constraints from
  // display in scenic_impl::ImportBufferCollection() and waited for successful allocation.
  {
    std::scoped_lock lock(lock_);
    buffer_collection_supports_display_[collection_id] = true;
    buffer_collection_pixel_format_modifier_[collection_id] =
        collection_info.settings().image_format_constraints().pixel_format_modifier();
    if (out_collection_info) {
      *out_collection_info = std::move(collection_info);
    }
  }

  std::vector<allocation::ImageMetadata> render_targets;
  for (uint32_t i = 0; i < num_render_targets; i++) {
    const allocation::ImageMetadata target = {.collection_id = collection_id,
                                              .identifier = allocation::GenerateUniqueImageId(),
                                              .vmo_index = i,
                                              .width = size.width,
                                              .height = size.height};
    render_targets.push_back(target);
    const bool res = ImportBufferImage(target, BufferCollectionUsage::kRenderTarget);
    FX_DCHECK(res);
  }
  return render_targets;
}

bool DisplayCompositor::ImportBufferCollectionToDisplayCoordinator(
    allocation::GlobalBufferCollectionId identifier,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token,
    const fuchsia_hardware_display_types::ImageBufferUsage& image_buffer_usage) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  return scenic_impl::ImportBufferCollection(identifier, display_coordinator_, std::move(token),
                                             image_buffer_usage);
}

}  // namespace flatland
