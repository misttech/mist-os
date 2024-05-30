// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "screen_capture_buffer_collection_importer.h"

#include <lib/async/default.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/status.h>

#include <cstdint>
#include <optional>
#include <utility>

#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"

namespace {

using allocation::BufferCollectionUsage;
// Image formats supported by Scenic in a priority order.
const vk::Format kSupportedImageFormats[] = {vk::Format::eR8G8B8A8Srgb, vk::Format::eB8G8R8A8Srgb};
}  // anonymous namespace

// Creates a new BufferCollectionTokenGroup from |token|. Then creates |num_tokens| number of
// children from |token_group|, calls AllChildrenPresent() and closes |token_group|.
std::optional<std::vector<fuchsia::sysmem2::BufferCollectionTokenHandle>> CreateChildTokens(
    const fuchsia::sysmem2::BufferCollectionTokenSyncPtr& token, uint32_t num_tokens) {
  fuchsia::sysmem2::BufferCollectionTokenGroupSyncPtr token_group;

  fuchsia::sysmem2::BufferCollectionTokenCreateBufferCollectionTokenGroupRequest
      create_group_request;
  create_group_request.set_group_request(token_group.NewRequest());
  zx_status_t status = token->CreateBufferCollectionTokenGroup(std::move(create_group_request));
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "Cannot create buffer collection token group: "
                     << zx_status_get_string(status);
    return std::nullopt;
  }

  fuchsia::sysmem2::Node_Sync_Result sync_result;
  status = token_group->Sync(&sync_result);
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "Cannot sync token group: " << zx_status_get_string(status);
    return std::nullopt;
  }

  std::vector<zx_rights_t> children_request_rights(num_tokens, ZX_RIGHT_SAME_RIGHTS);
  fuchsia::sysmem2::BufferCollectionTokenGroupCreateChildrenSyncRequest create_children_request;
  create_children_request.set_rights_attenuation_masks(std::move(children_request_rights));
  fuchsia::sysmem2::BufferCollectionTokenGroup_CreateChildrenSync_Result create_children_result;
  status =
      token_group->CreateChildrenSync(std::move(create_children_request), &create_children_result);
  if (status != ZX_OK || create_children_result.is_framework_err()) {
    FX_LOGS(WARNING) << "Cannot create buffer collection token group children: "
                     << zx_status_get_string(status)
                     << " is_framework_err: " << create_children_result.is_framework_err();
    return std::nullopt;
  }
  auto out_tokens = std::move(*create_children_result.response().mutable_tokens());

  status = token_group->AllChildrenPresent();
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "Could not call AllChildrenPresent: " << zx_status_get_string(status);
    return std::nullopt;
  }
  token_group->Release();

  return std::move(out_tokens);
}

// Consumes |token| to create a BufferCollectionSyncPtr and sets empty constraints on it.
std::optional<fuchsia::sysmem2::BufferCollectionSyncPtr>
CreateBufferCollectionSyncPtrAndSetEmptyConstraints(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token) {
  fuchsia::sysmem2::BufferCollectionSyncPtr local_buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(token));
  bind_shared_request.set_buffer_collection_request(local_buffer_collection.NewRequest());
  auto status = sysmem_allocator->BindSharedCollection(std::move(bind_shared_request));
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << __func__ << " failed, could not BindSharedCollection: " << status;
    return std::nullopt;
  }

  fuchsia::sysmem2::Node_Sync_Result sync_result;
  status = local_buffer_collection->Sync(&sync_result);
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << __func__ << " failed, could not bind buffer collection: " << status;
    return std::nullopt;
  }

  status = local_buffer_collection->SetConstraints(
      fuchsia::sysmem2::BufferCollectionSetConstraintsRequest{});
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << __func__ << " failed, could not set constraints: " << status;
    return std::nullopt;
  }
  return std::move(local_buffer_collection);
}

namespace screen_capture {

ScreenCaptureBufferCollectionImporter::ScreenCaptureBufferCollectionImporter(
    fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator,
    std::shared_ptr<flatland::Renderer> renderer)
    : sysmem_allocator_(std::move(sysmem_allocator)), renderer_(std::move(renderer)) {}

ScreenCaptureBufferCollectionImporter::~ScreenCaptureBufferCollectionImporter() {
  for (auto id : buffer_collections_) {
    renderer_->ReleaseBufferCollection(id, BufferCollectionUsage::kRenderTarget);
  }
  buffer_collections_.clear();
}

bool ScreenCaptureBufferCollectionImporter::ImportBufferCollection(
    allocation::GlobalBufferCollectionId collection_id,
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
    BufferCollectionUsage usage, std::optional<fuchsia::math::SizeU> size) {
  TRACE_DURATION("gfx", "ScreenCaptureBufferCollectionImporter::ImportBufferCollection");
  // Expect only RenderTarget usage.
  FX_DCHECK(usage == BufferCollectionUsage::kRenderTarget);

  if (!token.is_valid()) {
    FX_LOGS(WARNING) << "ImportBufferCollection called with invalid token";
    return false;
  }

  if (buffer_collections_.find(collection_id) != buffer_collections_.end()) {
    FX_LOGS(WARNING) << __func__ << " failed, called with pre-existing collection_id "
                     << collection_id << ".";
    return false;
  }

  // We are looking for a buffer that either satisfies render target requirements or readback
  // requirements. Buffer that satisfy render target and client requirements gives us a zero copy
  // path for screen capture, so it is preferred. If not, we fall back to readback requirements,
  // which is as minimal. To express this, we create a token group hierarchy defined below and
  // skip setting constraints on |local_token|.
  // * local_token / local_buffer_collection
  // . * token_group
  // . . * out_tokens[0] / render_target_token
  // . . * out_tokens[1] / readback_token
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr local_token = token.BindSync();
  auto child_tokens = CreateChildTokens(local_token, 2);
  if (!child_tokens.has_value()) {
    return false;
  }

  auto local_buffer_collection =
      CreateBufferCollectionSyncPtrAndSetEmptyConstraints(sysmem_allocator, std::move(local_token));
  if (!local_buffer_collection.has_value()) {
    return false;
  }

  auto render_target_token = std::move(child_tokens.value()[0]);
  if (!renderer_->ImportBufferCollection(collection_id, sysmem_allocator,
                                         std::move(render_target_token),
                                         BufferCollectionUsage::kRenderTarget, std::nullopt)) {
    FX_LOGS(WARNING) << "Could not register render target token with VkRenderer";
    return false;
  }

  auto readback_token = std::move(child_tokens.value()[1]);
  if (!renderer_->ImportBufferCollection(collection_id, sysmem_allocator, std::move(readback_token),
                                         BufferCollectionUsage::kReadback, std::nullopt)) {
    renderer_->ReleaseBufferCollection(collection_id, BufferCollectionUsage::kRenderTarget);
    FX_LOGS(WARNING) << "Could not register readback token with VkRenderer";
    return false;
  }

  buffer_collection_sync_ptrs_[collection_id] = std::move(local_buffer_collection.value());
  buffer_collections_.insert(collection_id);

  return true;
}

void ScreenCaptureBufferCollectionImporter::ReleaseBufferCollection(
    allocation::GlobalBufferCollectionId collection_id, BufferCollectionUsage usage) {
  TRACE_DURATION("gfx", "ScreenCaptureBufferCollectionImporter::ReleaseBufferCollection");

  // If the collection is not in the map, then there's nothing to do.
  if (buffer_collections_.find(collection_id) == buffer_collections_.end()) {
    FX_LOGS(WARNING) << "Attempting to release a non-existent buffer collection.";
    return;
  }

  buffer_collections_.erase(collection_id);
  reset_render_targets_.erase(collection_id);

  if (buffer_collection_sync_ptrs_.find(collection_id) != buffer_collection_sync_ptrs_.end()) {
    buffer_collection_sync_ptrs_.erase(collection_id);
  };

  if (buffer_collection_buffer_counts_.find(collection_id) !=
      buffer_collection_buffer_counts_.end()) {
    buffer_collection_buffer_counts_.erase(collection_id);
  };

  renderer_->ReleaseBufferCollection(collection_id, usage);
}

bool ScreenCaptureBufferCollectionImporter::ImportBufferImage(
    const allocation::ImageMetadata& metadata, BufferCollectionUsage usage) {
  TRACE_DURATION("gfx", "ScreenCaptureBufferCollectionImporter::ImportBufferImage");

  // The metadata can't have an invalid |collection_id|.
  if (metadata.collection_id == allocation::kInvalidId) {
    FX_LOGS(WARNING) << "Image has invalid collection id.";
    return false;
  }

  // The metadata can't have an invalid identifier.
  if (metadata.identifier == allocation::kInvalidImageId) {
    FX_LOGS(WARNING) << "Image has invalid identifier.";
    return false;
  }

  // Check for valid dimensions.
  if (metadata.width == 0 || metadata.height == 0) {
    FX_LOGS(WARNING) << "Image has invalid dimensions: "
                     << "(" << metadata.width << ", " << metadata.height << ").";
    return false;
  }

  // Make sure that the collection that will back this image's memory
  // is actually registered.
  auto collection_itr = buffer_collections_.find(metadata.collection_id);
  if (collection_itr == buffer_collections_.end()) {
    FX_LOGS(WARNING) << "Collection with id " << metadata.collection_id << " does not exist.";
    return false;
  }

  const std::optional<uint32_t> buffer_count =
      GetBufferCollectionBufferCount(metadata.collection_id);

  if (buffer_count == std::nullopt) {
    FX_LOGS(WARNING) << __func__ << " failed, buffer_count invalid";
    return false;
  }

  if (metadata.vmo_index >= buffer_count.value()) {
    FX_LOGS(WARNING) << __func__ << " failed, vmo_index " << metadata.vmo_index << " is invalid";
    return false;
  }

  FX_DCHECK(BufferCollectionUsage::kRenderTarget == usage);
  // Render target allocation failed, so we need to set the client buffer as a readback buffer.
  // Reset the imported buffer collections, reallocate a render target buffer and re-import.
  if (!renderer_->ImportBufferImage(metadata, BufferCollectionUsage::kRenderTarget)) {
    if (!ResetRenderTargetsForReadback(metadata, buffer_count.value())) {
      FX_LOGS(WARNING) << "Cannot reallocate readback render targets!";
      return false;
    }
    if (!renderer_->ImportBufferImage(metadata, BufferCollectionUsage::kReadback)) {
      FX_LOGS(WARNING) << "Could not import fallback render target to VkRenderer";
      return false;
    }
    if (!renderer_->ImportBufferImage(metadata, BufferCollectionUsage::kRenderTarget)) {
      FX_LOGS(WARNING) << "Could not import fallback render target to VkRenderer";
      return false;
    }
  } else {
    // Render target allocation succeeded. We can use the client buffer as render target and there
    // is no need for readback buffers.
    if (reset_render_targets_.find(metadata.collection_id) == reset_render_targets_.end()) {
      renderer_->ReleaseBufferCollection(metadata.collection_id, BufferCollectionUsage::kReadback);
    } else {
      // Render target allocation succeeded on a buffer, where ResetRenderTargetsForReadback() was
      // called, so we need to set the client buffer as a readback buffer.
      renderer_->ImportBufferImage(metadata, BufferCollectionUsage::kReadback);
    }
  }

  return true;
}

void ScreenCaptureBufferCollectionImporter::ReleaseBufferImage(allocation::GlobalImageId image_id) {
  TRACE_DURATION("gfx", "ScreenCaptureBufferCollectionImporter::ReleaseBufferImage");
  renderer_->ReleaseBufferImage(image_id);
}

std::optional<BufferCount> ScreenCaptureBufferCollectionImporter::GetBufferCollectionBufferCount(
    allocation::GlobalBufferCollectionId collection_id) {
  // If the collection info has not been retrieved before, wait for the buffers to be allocated
  // and populate the map/delete the reference to the |collection_id| from
  // |collection_id_sync_ptrs_|.
  if (buffer_collection_buffer_counts_.find(collection_id) ==
      buffer_collection_buffer_counts_.end()) {
    fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;

    if (buffer_collection_sync_ptrs_.find(collection_id) == buffer_collection_sync_ptrs_.end()) {
      FX_LOGS(WARNING) << "Collection with id " << collection_id << " does not exist.";
      return std::nullopt;
    }

    buffer_collection = std::move(buffer_collection_sync_ptrs_[collection_id]);

    fuchsia::sysmem2::BufferCollection_CheckAllBuffersAllocated_Result check_allocated_result;
    zx_status_t status = buffer_collection->CheckAllBuffersAllocated(&check_allocated_result);
    if (status != ZX_OK) {
      FX_LOGS(WARNING) << __func__ << " failed, no buffers allocated - status: " << status;
      return std::nullopt;
    }
    if (check_allocated_result.is_framework_err()) {
      FX_LOGS(WARNING) << __func__ << " failed, no buffers allocated - framework_err: "
                       << fidl::ToUnderlying(check_allocated_result.framework_err());
      return std::nullopt;
    }
    if (check_allocated_result.is_err()) {
      ZX_DEBUG_ASSERT(check_allocated_result.is_err());
      FX_LOGS(WARNING) << __func__ << " failed, no buffers allocated - err: "
                       << static_cast<uint32_t>(check_allocated_result.err());
      return std::nullopt;
    }

    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
    if (status != ZX_OK) {
      FX_LOGS(WARNING) << __func__
                       << " failed, waiting on no buffers allocated - status: " << status;
      return std::nullopt;
    }
    if (wait_result.is_framework_err()) {
      FX_LOGS(WARNING) << __func__ << " failed, waiting on no buffers allocated - framework_err: "
                       << fidl::ToUnderlying(wait_result.framework_err());
      return std::nullopt;
    }
    if (wait_result.is_err()) {
      FX_LOGS(WARNING) << __func__ << " failed, waiting on no buffers allocated - err: "
                       << static_cast<uint32_t>(wait_result.framework_err());
      return std::nullopt;
    }
    auto buffer_collection_info =
        std::move(*wait_result.response().mutable_buffer_collection_info());

    buffer_collection_sync_ptrs_.erase(collection_id);
    buffer_collection->Release();

    buffer_collection_buffer_counts_[collection_id] = buffer_collection_info.buffers().size();
  }

  return buffer_collection_buffer_counts_[collection_id];
}

bool ScreenCaptureBufferCollectionImporter::ResetRenderTargetsForReadback(
    const allocation::ImageMetadata& metadata, uint32_t buffer_count) {
  // Resetting render target for readback only should happen once at the first ImportBufferImage
  // from that BufferCollection. Don't do it again if this method had already been called for this
  // |metadata.collection_id|.
  if (reset_render_targets_.find(metadata.collection_id) != reset_render_targets_.end()) {
    return true;
  }

  FX_LOGS(INFO) << "Could not import render target to VkRenderer; attempting to create fallback";
  renderer_->ReleaseBufferCollection(metadata.collection_id, BufferCollectionUsage::kRenderTarget);

  auto deregister_collection =
      fit::defer([renderer = renderer_, collection_id = metadata.collection_id] {
        renderer->ReleaseBufferCollection(collection_id, BufferCollectionUsage::kReadback);
      });

  fuchsia::sysmem2::BufferCollectionTokenSyncPtr fallback_render_target_sync_token;
  fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.set_token_request(fallback_render_target_sync_token.NewRequest());
  zx_status_t status =
      sysmem_allocator_->AllocateSharedCollection(std::move(allocate_shared_request));
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "Cannot allocate fallback render target sync token: "
                     << zx_status_get_string(status);
    return false;
  }

  fuchsia::sysmem2::BufferCollectionTokenHandle fallback_render_target_token;
  fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest dup_request;
  dup_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
  dup_request.set_token_request(fallback_render_target_token.NewRequest());
  status = fallback_render_target_sync_token->Duplicate(std::move(dup_request));
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Cannot duplicate fallback render target sync token: "
                   << zx_status_get_string(status);
    return false;
  }

  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(fallback_render_target_sync_token));
  bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
  status = sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request));
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Cannot bind fallback render target sync token: "
                   << zx_status_get_string(status);
    return false;
  }

  if (!renderer_->ImportBufferCollection(
          metadata.collection_id, sysmem_allocator_.get(), std::move(fallback_render_target_token),
          BufferCollectionUsage::kRenderTarget,
          std::optional<fuchsia::math::SizeU>({metadata.width, metadata.height}))) {
    FX_LOGS(WARNING) << "Could not register fallback render target with VkRenderer";
    return false;
  }

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  auto& constraints = *set_constraints_request.mutable_constraints();
  constraints.set_min_buffer_count(buffer_count);
  constraints.mutable_usage()->set_none(fuchsia::sysmem2::NONE_USAGE);
  status = buffer_collection->SetConstraints(std::move(set_constraints_request));
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "Cannot set constraints on fallback render target collection: "
                     << zx_status_get_string(status);
    return false;
  }

  fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
  status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
  if (status != ZX_OK) {
    FX_LOGS(WARNING)
        << "Could not wait on allocation for fallback render target collection - status: "
        << zx_status_get_string(status);
    return false;
  }
  if (wait_result.is_framework_err()) {
    FX_LOGS(WARNING)
        << "Could not wait on allocation for fallback render target collection - framework_err: "
        << fidl::ToUnderlying(wait_result.framework_err());
    return false;
  }
  if (wait_result.is_err()) {
    FX_LOGS(WARNING) << "Could not wait on allocation for fallback render target collection - err: "
                     << static_cast<uint32_t>(wait_result.err());
    return false;
  }

  status = buffer_collection->Release();
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "Could not close fallback render target collection: "
                     << zx_status_get_string(status);
    return false;
  }

  reset_render_targets_.insert(metadata.collection_id);
  deregister_collection.cancel();
  return true;
}

}  // namespace screen_capture
