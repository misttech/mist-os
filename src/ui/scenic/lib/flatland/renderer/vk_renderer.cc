// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/renderer/vk_renderer.h"

#include <fidl/fuchsia.ui.composition/cpp/hlcpp_conversion.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include "src/ui/lib/escher/escher.h"
#include "src/ui/lib/escher/flatland/rectangle_compositor.h"
#include "src/ui/lib/escher/forward_declarations.h"
#include "src/ui/lib/escher/impl/naive_image.h"
#include "src/ui/lib/escher/impl/vulkan_utils.h"
#include "src/ui/lib/escher/renderer/batch_gpu_uploader.h"
#include "src/ui/lib/escher/renderer/render_funcs.h"
#include "src/ui/lib/escher/renderer/sampler_cache.h"
#include "src/ui/lib/escher/resources/resource_recycler.h"
#include "src/ui/lib/escher/third_party/granite/vk/command_buffer.h"
#include "src/ui/lib/escher/util/fuchsia_utils.h"
#include "src/ui/lib/escher/util/image_utils.h"
#include "src/ui/lib/escher/util/trace_macros.h"
#include "src/ui/scenic/lib/utils/shader_warmup.h"

#include <glm/glm.hpp>
#include <glm/gtc/type_ptr.hpp>
#include <vulkan/vulkan.hpp>

namespace {

using allocation::BufferCollectionUsage;
using fuchsia_ui_composition::ImageFlip;

// TODO(https://fxbug.dev/42072347): We support two framebuffer formats and warmup for both.
// * RGBA is the only supported format for AFBC on mali. Should be the default in production.
// * BGRA is the only format that allows screen capture and testing on mali. It is also used as
// default on screenshots.
const std::vector<vk::Format> kSupportedRenderTargetImageFormats = {vk::Format::eR8G8B8A8Srgb,
                                                                    vk::Format::eB8G8R8A8Srgb};

const std::vector<vk::Format> kSupportedReadbackImageFormats = {vk::Format::eR8G8B8A8Srgb,
                                                                vk::Format::eB8G8R8A8Srgb};

const std::vector<vk::Format>& GetSupportedImageFormatsForBufferCollectionUsage(
    BufferCollectionUsage usage) {
  switch (usage) {
    case BufferCollectionUsage::kClientImage:
      return utils::SupportedClientImageFormats();
      break;
    case BufferCollectionUsage::kRenderTarget:
      return kSupportedRenderTargetImageFormats;
      break;
    case BufferCollectionUsage::kReadback:
      return kSupportedReadbackImageFormats;
      break;
  }
}

const vk::Filter kDefaultFilter = vk::Filter::eLinear;

// Black color to replace protected content when we aren't in protected mode, i.e. Screenshots.
const glm::vec4 kProtectedReplacementColorInRGBA = glm::vec4(0, 0, 0, 1);

// Returns the corresponding Vulkan image format to use given the provided
// Zircon image format.
vk::Format ConvertToVkFormat(const fuchsia_images2::PixelFormat pixel_format) {
  switch (pixel_format) {
    // These two Zircon formats correspond to the Sysmem BGRA32 format.
    case fuchsia_images2::PixelFormat::kB8G8R8A8:
      return vk::Format::eB8G8R8A8Srgb;
    // These two Zircon formats correspond to the Sysmem R8G8B8A8 format.
    case fuchsia_images2::PixelFormat::kR8G8B8A8:
      return vk::Format::eR8G8B8A8Srgb;
    case fuchsia_images2::PixelFormat::kNv12:
      return vk::Format::eG8B8R82Plane420Unorm;
    case fuchsia_images2::PixelFormat::kR5G6B5:
      return vk::Format::eR5G6B5UnormPack16;
    default:
      FX_CHECK(false) << "Unsupported Zircon pixel format: " << static_cast<uint32_t>(pixel_format);
      return vk::Format::eUndefined;
  }
}

// Create a default 1x1 texture for solid color renderables which are not associated
// with an image.
escher::TexturePtr CreateWhiteTexture(escher::Escher* escher,
                                      escher::BatchGpuUploader* gpu_uploader) {
  FX_DCHECK(escher);
  uint8_t channels[4];
  channels[0] = channels[1] = channels[2] = channels[3] = 255;
  auto image = escher->NewRgbaImage(gpu_uploader, 1, 1, channels);
  return escher->NewTexture(std::move(image), vk::Filter::eNearest);
}

escher::TexturePtr CreateDepthTexture(escher::Escher* escher,
                                      const escher::ImagePtr& output_image) {
  escher::TexturePtr depth_buffer;
  escher::RenderFuncs::ObtainDepthTexture(
      escher, output_image->use_protected_memory(), output_image->info(),
      escher->device()->caps().GetMatchingDepthStencilFormat().value, depth_buffer);
  return depth_buffer;
}

constexpr float clamp(float v, float lo, float hi) { return (v < lo) ? lo : (hi < v) ? hi : v; }

std::array<size_t, 4> GetFlippedIndices(const ImageFlip flip_type) {
  switch (flip_type) {
    case ImageFlip::kNone:
      return {0, 1, 2, 3};
    case ImageFlip::kLeftRight:
      // The indices are sorted in a clockwise order starting at the top-left, and the left
      // indices must be swapped with the right.
      return {1, 0, 3, 2};
    case ImageFlip::kUpDown:
      // The indices are sorted in a clockwise order starting at the top-left, and the top indices
      // must be swapped with the bottom.
      return {3, 2, 1, 0};
    default:
      FX_NOTREACHED();
      return {0, 0, 0, 0};
  }
}

std::array<glm::ivec2, 4> FlipUVs(const std::array<glm::ivec2, 4>& uvs, const ImageFlip flip_type) {
  const std::array<size_t, 4> flip_indices = GetFlippedIndices(flip_type);
  std::array<glm::ivec2, 4> flipped_uvs;
  for (size_t i = 0; i < 4; i++) {
    flipped_uvs[i] = uvs[flip_indices[i]];
  }
  return flipped_uvs;
}

std::vector<escher::Rectangle2D> GetNormalizedUvRects(
    const std::vector<flatland::ImageRect>& rectangles,
    const std::vector<flatland::ImageMetadata>& images) {
  FX_DCHECK(rectangles.size() == images.size());

  std::vector<escher::Rectangle2D> normalized_rects;

  for (unsigned int i = 0; i < rectangles.size(); i++) {
    const auto& rect = rectangles[i];
    const auto& image = images[i];
    const auto& orientation = rectangles[i].orientation;
    const float w = static_cast<float>(image.width);
    const float h = static_cast<float>(image.height);
    FX_DCHECK(w >= 0.f && h >= 0.f);

    // First, reorder the UVs based on whether the image was flipped.
    const auto texel_uvs = FlipUVs(rectangles[i].texel_uvs, image.flip);

    // Reorder based on rotation and normalize the texel UVs. Normalization is based on the width
    // and height of the image that is sampled from. Reordering is based on orientation. The texel
    // UVs are listed in clockwise-order starting at the top-left corner of the texture. They need
    // to be reordered so that they are listed in clockwise-order and the UV that maps to the
    // top-left corner of the escher::Rectangle2D is listed first. For instance, if the rectangle is
    // rotated 90_CCW, the first texel UV of the ImageRect, at index 0, is at index 3 in the
    // escher::Rectangle2D.
    std::array<glm::vec2, 4> normalized_uvs;
    // |fuchsia::ui::composition::Orientation| is an enum value in the range [1, 4].
    int starting_index = static_cast<int>(orientation) - 1;
    for (int j = 0; j < 4; j++) {
      const int index = (starting_index + j) % 4;
      // Clamp values to ensure they are normalized to the range [0, 1].
      normalized_uvs[j] = glm::vec2(clamp(static_cast<float>(texel_uvs[index].x), 0, w) / w,
                                    clamp(static_cast<float>(texel_uvs[index].y), 0, h) / h);
    }

    normalized_rects.push_back({rect.origin, rect.extent, normalized_uvs});
  }

  return normalized_rects;
}

std::atomic<uint64_t> next_buffer_collection_id = 1;

uint64_t GetNextBufferCollectionId() { return next_buffer_collection_id++; }

std::string GetNextBufferCollectionIdString(const char* prefix) {
  // Would use std::ostringstream here, except it bloats binary size by ~50kB, causing CQ to fail.
  return std::string(prefix) + "-" + std::to_string(GetNextBufferCollectionId());
}

std::string GetImageName(const BufferCollectionUsage usage) {
  switch (usage) {
    case BufferCollectionUsage::kRenderTarget:
      return "FlatlandRenderTargetMemory";
    case BufferCollectionUsage::kReadback:
      return "FlatlandReadbackMemory";
    case BufferCollectionUsage::kClientImage:
      return "FlatlandImageMemory";
    default:
      FX_NOTREACHED();
      return "";
  }
}

vk::ImageUsageFlags GetImageUsageFlags(const BufferCollectionUsage usage) {
  switch (usage) {
    case BufferCollectionUsage::kRenderTarget:
      return escher::RectangleCompositor::kRenderTargetUsageFlags |
             vk::ImageUsageFlagBits::eTransferSrc;
    case BufferCollectionUsage::kReadback:
      return vk::ImageUsageFlagBits::eTransferDst;
    case BufferCollectionUsage::kClientImage:
      return escher::RectangleCompositor::kTextureUsageFlags;
    default:
      FX_NOTREACHED();
      return static_cast<vk::ImageUsageFlags>(0);
  }
}

// Creates a duplicate of |token|. Returns a std::nullopt if it fails.
std::optional<fuchsia::sysmem2::BufferCollectionTokenSyncPtr> Duplicate(
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr& token) {
  fuchsia::sysmem2::BufferCollectionTokenDuplicateSyncRequest dup_sync_request;
  dup_sync_request.set_rights_attenuation_masks({ZX_RIGHT_SAME_RIGHTS});
  fuchsia::sysmem2::BufferCollectionToken_DuplicateSync_Result dup_sync_result;
  const auto status = token->DuplicateSync(std::move(dup_sync_request), &dup_sync_result);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Could not duplicate token: " << zx_status_get_string(status);
    return std::nullopt;
  }
  if (dup_sync_result.is_framework_err()) {
    FX_LOGS(ERROR) << "Could not duplicate token (framework): "
                   << fidl::ToUnderlying(dup_sync_result.framework_err());
    return std::nullopt;
  }
  FX_DCHECK(dup_sync_result.response().tokens().size() == 1);
  return dup_sync_result.response().mutable_tokens()->front().BindSync();
}

std::optional<fuchsia::sysmem2::BufferCollectionSyncPtr>
CreateBufferCollectionPtrWithEmptyConstraints(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token) {
  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(token));
  bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
  if (const zx_status_t status =
          sysmem_allocator->BindSharedCollection(std::move(bind_shared_request));
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Could not bind buffer collection: " << zx_status_get_string(status);
    return std::nullopt;
  }

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  // intentionally don't set the constraints field of the request
  if (const zx_status_t status =
          buffer_collection->SetConstraints(std::move(set_constraints_request));
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Cannot set constraints: " << zx_status_get_string(status);
    return std::nullopt;
  }

  return buffer_collection;
}

std::vector<vk::ImageFormatConstraintsInfoFUCHSIA> GetVulkanImageFormatConstraints(
    const BufferCollectionUsage usage, const std::optional<fuchsia::math::SizeU> size) {
  std::vector<vk::ImageFormatConstraintsInfoFUCHSIA> constraint_infos;
  for (const auto& format : GetSupportedImageFormatsForBufferCollectionUsage(usage)) {
    vk::ImageCreateInfo create_info =
        escher::RectangleCompositor::GetDefaultImageConstraints(format, GetImageUsageFlags(usage));
    if (size.has_value() && size.value().width && size.value().height) {
      create_info.extent = vk::Extent3D{size.value().width, size.value().height, 1};
    }

    constraint_infos.push_back(escher::GetDefaultImageFormatConstraintsInfo(create_info));
  }

  return constraint_infos;
}

bool IsValidImage(const allocation::ImageMetadata& metadata) {
  // The metadata can't have an invalid collection id.
  if (metadata.collection_id == allocation::kInvalidId) {
    FX_LOGS(WARNING) << "Image has invalid collection id.";
    return false;
  }

  // The metadata can't have an invalid identifier.
  if (metadata.identifier == allocation::kInvalidImageId) {
    FX_LOGS(WARNING) << "Image has invalid identifier.";
    return false;
  }

  // Check we have valid dimensions.
  if (metadata.width == 0 || metadata.height == 0) {
    FX_LOGS(WARNING) << "Image has invalid dimensions: "
                     << "(" << metadata.width << ", " << metadata.height << ").";
    return false;
  }

  return true;
}

}  // anonymous namespace

namespace flatland {

VkRenderer::VkRenderer(escher::EscherWeakPtr escher)
    : escher_(std::move(escher)),
      compositor_(escher::RectangleCompositor(escher_)),
      main_dispatcher_(async_get_default_dispatcher()) {
  auto gpu_uploader = escher::BatchGpuUploader::New(escher_, /*frame_trace_number*/ 0);
  FX_DCHECK(gpu_uploader);

  texture_map_[allocation::kInvalidImageId] = CreateWhiteTexture(escher_.get(), gpu_uploader.get());
  gpu_uploader->Submit();

  {
    TRACE_DURATION("gfx", "VkRenderer::Initialize");
    WaitIdle();
  }
}

VkRenderer::~VkRenderer() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());

  auto vk_device = escher_->vk_device();
  auto vk_loader = escher_->device()->dispatch_loader();
  for (auto& [_, collection] : texture_collections_) {
    vk_device.destroyBufferCollectionFUCHSIA(collection.vk_collection, nullptr, vk_loader);
  }
  for (auto& [_, collection] : render_target_collections_) {
    vk_device.destroyBufferCollectionFUCHSIA(collection.vk_collection, nullptr, vk_loader);
  }
  render_target_collections_.clear();
  for (auto& [_, collection] : readback_collections_) {
    vk_device.destroyBufferCollectionFUCHSIA(collection.vk_collection, nullptr, vk_loader);
  }
  readback_collections_.clear();
}

std::optional<vk::BufferCollectionFUCHSIA>
VkRenderer::SetConstraintsAndCreateVulkanBufferCollection(
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token, const BufferCollectionUsage usage,
    const std::optional<fuchsia::math::SizeU> size) {
  auto vk_device = escher_->vk_device();
  auto vk_loader = escher_->device()->dispatch_loader();
  FX_DCHECK(vk_device);

  vk::BufferCollectionCreateInfoFUCHSIA bc_create_info;
  bc_create_info.collectionToken = token.Unbind().TakeChannel().release();
  const vk::BufferCollectionFUCHSIA vk_collection = escher::ESCHER_CHECKED_VK_RESULT(
      vk_device.createBufferCollectionFUCHSIA(bc_create_info, nullptr, vk_loader));

  vk::ImageConstraintsInfoFUCHSIA vk_image_constraints;
  const auto image_format_constraints = GetVulkanImageFormatConstraints(usage, size);
  vk_image_constraints.setFormatConstraints(image_format_constraints)
      .setFlags(escher_->allow_protected_memory()
                    ? vk::ImageConstraintsInfoFlagBitsFUCHSIA::eProtectedOptional
                    : vk::ImageConstraintsInfoFlagsFUCHSIA{})
      .setBufferCollectionConstraints(
          vk::BufferCollectionConstraintsInfoFUCHSIA().setMinBufferCount(1u));

  if (const auto vk_result = vk_device.setBufferCollectionImageConstraintsFUCHSIA(
          vk_collection, vk_image_constraints, vk_loader);
      vk_result != vk::Result::eSuccess) {
    FX_LOGS(ERROR) << "Cannot set vulkan constraints: " << vk::to_string(vk_result)
                   << "; The client may have invalidated the token.";
    return std::nullopt;
  }

  return vk_collection;
}

std::optional<vk::BufferCollectionFUCHSIA> VkRenderer::GetAllocatedVulkanBufferCollection(
    const allocation::GlobalBufferCollectionId collection_id, const BufferCollectionUsage usage) {
  std::scoped_lock lock(lock_);
  // Make sure that the collection that will back this image's memory
  // is actually registered with the renderer.
  std::unordered_map<GlobalBufferCollectionId, CollectionData>& collections =
      GetBufferCollectionsFor(usage);
  auto collection_itr = collections.find(collection_id);
  if (collection_itr == collections.end()) {
    FX_LOGS(WARNING) << "Collection with id " << collection_id << " does not exist.";
    return std::nullopt;
  }

  auto& [collection, vk_collection, is_allocated] = collection_itr->second;
  // If we've checked the allocation before we don't need to do so again.
  if (is_allocated) {
    return vk_collection;
  }

  // Check to see if the buffers are allocated and return std::nullptr if not.
  fuchsia::sysmem2::BufferCollection_CheckAllBuffersAllocated_Result check_result;
  if (const zx_status_t status = collection->CheckAllBuffersAllocated(&check_result);
      status != ZX_OK) {
    FX_LOGS(WARNING) << "Collection was not allocated (FIDL status: "
                     << zx_status_get_string(status) << ").";
    return std::nullopt;
  }
  if (!check_result.is_response()) {
    if (check_result.is_framework_err()) {
      FX_LOGS(WARNING) << "Collection was not allocated (framework err): "
                       << fidl::ToUnderlying(check_result.framework_err());
    } else {
      FX_LOGS(WARNING) << "Collection was not allocated (allocation Error: "
                       << static_cast<uint32_t>(check_result.err()) << ").";
    }
    return std::nullopt;
  }

  is_allocated = true;
  return vk_collection;
}

bool VkRenderer::ImportBufferCollection(
    GlobalBufferCollectionId collection_id, fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
    BufferCollectionUsage usage, std::optional<fuchsia::math::SizeU> size) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "flatland::VkRenderer::ImportBufferCollection");
  FX_DCHECK(collection_id != allocation::kInvalidId);
  FX_DCHECK(token.is_valid());

  // TODO(https://fxbug.dev/42128380): See if this can become asynchronous.
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr local_token = token.BindSync();
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr vulkan_token;
  if (auto dup_token = Duplicate(local_token)) {
    vulkan_token = std::move(*dup_token);
  } else {
    return false;
  }

  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  if (auto collection =
          CreateBufferCollectionPtrWithEmptyConstraints(sysmem_allocator, std::move(local_token))) {
    buffer_collection = std::move(*collection);
    // Use a name with a priority that's greater than the vulkan implementation, but less than
    // what any client would use.
    fuchsia::sysmem2::NodeSetNameRequest set_name_request;
    set_name_request.set_priority(10u);
    set_name_request.set_name(GetNextBufferCollectionIdString(GetImageName(usage).c_str()));
    buffer_collection->SetName(std::move(set_name_request));
  } else {
    return false;
  }

  vk::BufferCollectionFUCHSIA vk_collection;
  if (const auto collection =
          SetConstraintsAndCreateVulkanBufferCollection(std::move(vulkan_token), usage, size)) {
    vk_collection = std::move(*collection);
  } else {
    return false;
  }

  // TODO(https://fxbug.dev/42120738): Convert this to a lock-free structure.
  std::scoped_lock lock(lock_);

  std::unordered_map<GlobalBufferCollectionId, CollectionData>& collections =
      GetBufferCollectionsFor(usage);
  const auto [_, emplace_success] = collections.emplace(
      std::make_pair(collection_id, CollectionData{.collection = std::move(buffer_collection),
                                                   .vk_collection = std::move(vk_collection)}));
  if (!emplace_success) {
    FX_LOGS(WARNING) << "Could not store buffer collection, because an entry already existed for "
                     << collection_id;
    return false;
  }

  return true;
}

void VkRenderer::ReleaseBufferCollection(GlobalBufferCollectionId collection_id,
                                         BufferCollectionUsage usage) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "flatland::VkRenderer::ReleaseBufferCollection");

  // TODO(https://fxbug.dev/42120738): Convert this to a lock-free structure.
  std::scoped_lock lock(lock_);

  std::unordered_map<GlobalBufferCollectionId, CollectionData>& collections =
      GetBufferCollectionsFor(usage);
  const auto collection_itr = collections.find(collection_id);

  // If the collection is not in the map, then there's nothing to do.
  if (collection_itr == collections.end()) {
    FX_LOGS(WARNING) << "Attempting to release a non-existent buffer collection.";
    return;
  }

  auto vk_device = escher_->vk_device();
  auto vk_loader = escher_->device()->dispatch_loader();
  vk_device.destroyBufferCollectionFUCHSIA(collection_itr->second.vk_collection, nullptr,
                                           vk_loader);

  const zx_status_t status = collection_itr->second.collection->Release();
  // AttachToken failure causes ZX_ERR_PEER_CLOSED.
  if (status != ZX_OK && status != ZX_ERR_PEER_CLOSED) {
    FX_LOGS(ERROR) << "Error when closing buffer collection: " << zx_status_get_string(status);
  }

  collections.erase(collection_itr);
}

bool VkRenderer::ImageIsAlreadyRegisteredForUsage(const allocation::GlobalImageId image_id,
                                                  const BufferCollectionUsage usage) {
  std::scoped_lock lock(lock_);
  switch (usage) {
    case BufferCollectionUsage::kRenderTarget:
      return render_target_map_.find(image_id) != render_target_map_.end();
    case BufferCollectionUsage::kReadback:
      return readback_image_map_.find(image_id) != readback_image_map_.end();
    case BufferCollectionUsage::kClientImage:
      return texture_map_.find(image_id) != texture_map_.end();
    default:
      FX_NOTREACHED();
      return false;
  }
}
bool VkRenderer::ImportRenderTargetImage(const allocation::ImageMetadata& metadata,
                                         const vk::BufferCollectionFUCHSIA vk_collection) {
  bool needs_readback = false;
  {
    std::scoped_lock lock(lock_);
    needs_readback =
        readback_collections_.find(metadata.collection_id) != readback_collections_.end();
  }

  const vk::ImageUsageFlags kRenderTargetAsReadbackSourceUsageFlags =
      escher::RectangleCompositor::kRenderTargetUsageFlags | vk::ImageUsageFlagBits::eTransferSrc;
  const auto image =
      ExtractImage(metadata, BufferCollectionUsage::kRenderTarget, vk_collection,
                   needs_readback ? kRenderTargetAsReadbackSourceUsageFlags
                                  : escher::RectangleCompositor::kRenderTargetUsageFlags);
  if (!image) {
    FX_LOGS(ERROR) << "Could not extract render target.";
    return false;
  }

  image->set_swapchain_layout(vk::ImageLayout::eColorAttachmentOptimal);
  auto depth_texture = CreateDepthTexture(escher_.get(), image);

  std::scoped_lock lock(lock_);
  render_target_map_[metadata.identifier] = image;
  depth_target_map_[metadata.identifier] = std::move(depth_texture);
  pending_render_targets_.insert(metadata.identifier);
  return true;
}

bool VkRenderer::ImportReadbackImage(const allocation::ImageMetadata& metadata,
                                     const vk::BufferCollectionFUCHSIA vk_collection) {
  const escher::ImagePtr readback_image =
      ExtractImage(metadata, BufferCollectionUsage::kReadback, vk_collection,
                   vk::ImageUsageFlagBits::eTransferDst);
  if (!readback_image) {
    FX_LOGS(ERROR) << "Could not extract readback image.";
    return false;
  }

  std::scoped_lock lock(lock_);
  readback_image_map_[metadata.identifier] = readback_image;
  return true;
}

bool VkRenderer::ImportClientImage(const allocation::ImageMetadata& metadata,
                                   const vk::BufferCollectionFUCHSIA vk_collection) {
  const auto texture = ExtractTexture(metadata, vk_collection);
  if (!texture) {
    FX_LOGS(ERROR) << "Could not extract client texture image.";
    return false;
  }

  std::scoped_lock lock(lock_);
  texture_map_[metadata.identifier] = texture;
  pending_textures_.insert(metadata.identifier);
  return true;
}

bool VkRenderer::ImportBufferImage(const allocation::ImageMetadata& metadata,
                                   const BufferCollectionUsage usage) {
  TRACE_DURATION("gfx", "flatland::VkRenderer::ImportBufferImage");

  if (!IsValidImage(metadata)) {
    return false;
  }

  vk::BufferCollectionFUCHSIA vk_collection;
  if (const auto collection = GetAllocatedVulkanBufferCollection(metadata.collection_id, usage)) {
    vk_collection = *collection;
  } else {
    return false;
  }

  if (ImageIsAlreadyRegisteredForUsage(metadata.identifier, usage)) {
    FX_LOGS(WARNING) << "An image with identifier " << metadata.identifier
                     << " has already been registered for usage: " << static_cast<uint32_t>(usage);
    return false;
  }

  bool import_result = false;
  switch (usage) {
    case BufferCollectionUsage::kRenderTarget: {
      import_result = ImportRenderTargetImage(metadata, vk_collection);
      break;
    }
    case BufferCollectionUsage::kReadback: {
      import_result = ImportReadbackImage(metadata, vk_collection);
      break;
    }
    case BufferCollectionUsage::kClientImage: {
      import_result = ImportClientImage(metadata, vk_collection);
      break;
    }
    default:
      FX_NOTREACHED();
  }
  return import_result;
}

void VkRenderer::ReleaseBufferImage(allocation::GlobalImageId image_id) {
  // Called from main thread or Flatland threads.
  TRACE_DURATION("gfx", "flatland::VkRenderer::ReleaseBufferImage");
  FX_DCHECK(image_id != allocation::kInvalidImageId);

  std::scoped_lock lock(lock_);

  if (texture_map_.find(image_id) != texture_map_.end()) {
    texture_map_.erase(image_id);
    pending_textures_.erase(image_id);
  } else if (render_target_map_.find(image_id) != render_target_map_.end()) {
    render_target_map_.erase(image_id);
    depth_target_map_.erase(image_id);
    readback_image_map_.erase(image_id);
    pending_render_targets_.erase(image_id);
  }
}

escher::ImagePtr VkRenderer::ExtractImage(const allocation::ImageMetadata& metadata,
                                          const BufferCollectionUsage bc_usage,
                                          const vk::BufferCollectionFUCHSIA collection,
                                          const vk::ImageUsageFlags image_usage,
                                          const bool readback) {
  // Called from main thread or Flatland threads.
  TRACE_DURATION("gfx", "VkRenderer::ExtractImage");
  auto vk_device = escher_->vk_device();
  auto vk_loader = escher_->device()->dispatch_loader();

  // Grab the collection Properties from Vulkan.
  // TODO(https://fxbug.dev/42053219): Add unittests to cover the case where sysmem client
  // token gets invalidated when importing images.
  vk::BufferCollectionPropertiesFUCHSIA properties;
  if (const auto properties_results =
          vk_device.getBufferCollectionPropertiesFUCHSIA(collection, vk_loader);
      properties_results.result == vk::Result::eSuccess) {
    properties = properties_results.value;
  } else {
    FX_LOGS(WARNING) << "Could not get buffer collection properties: "
                     << vk::to_string(properties_results.result);
    return nullptr;
  }

  // Check the provided index against actually allocated number of buffers.
  if (properties.bufferCount <= metadata.vmo_index) {
    FX_LOGS(ERROR) << "Specified vmo index is out of bounds: " << metadata.vmo_index;
    return nullptr;
  }

  // Check if allocated buffers are backed by protected memory.
  const bool is_protected =
      (escher_->vk_physical_device()
           .getMemoryProperties()
           .memoryTypes[escher::CountTrailingZeros(properties.memoryTypeBits)]
           .propertyFlags &
       vk::MemoryPropertyFlagBits::eProtected) == vk::MemoryPropertyFlagBits::eProtected;

  // Setup the create info Fuchsia extension.
  vk::BufferCollectionImageCreateInfoFUCHSIA collection_image_info;
  collection_image_info.collection = collection;
  collection_image_info.index = metadata.vmo_index;

  // Setup the create info.
  const auto& kSupportedImageFormats = GetSupportedImageFormatsForBufferCollectionUsage(bc_usage);

  // The same list of formats was provided when specifying constraints in ImportBufferCollection();
  // |createInfoIndex| is the index into the list, in the same order that it was provided.
  FX_DCHECK(properties.createInfoIndex < std::size(kSupportedImageFormats));
  const auto pixel_format = kSupportedImageFormats[properties.createInfoIndex];
  vk::ImageCreateInfo create_info =
      escher::RectangleCompositor::GetDefaultImageConstraints(pixel_format, image_usage);
  create_info.extent = vk::Extent3D{metadata.width, metadata.height, 1};
  create_info.setPNext(&collection_image_info);
  if (is_protected) {
    create_info.flags = vk::ImageCreateFlagBits::eProtected;
  }

  // Create the VK Image, return nullptr if this fails.
  vk::Image image;
  if (const auto image_result = vk_device.createImage(create_info);
      image_result.result == vk::Result::eSuccess) {
    image = image_result.value;
  } else {
    FX_LOGS(ERROR) << "VkCreateImage failed: " << vk::to_string(image_result.result);
    return nullptr;
  }

  // Now we have to allocate VK memory for the image. This memory is going to come from
  // the imported buffer collection's vmo.
  const auto memory_requirements = vk_device.getImageMemoryRequirements(image);
  const uint32_t memory_type_index =
      escher::CountTrailingZeros(memory_requirements.memoryTypeBits & properties.memoryTypeBits);
  const vk::StructureChain<vk::MemoryAllocateInfo, vk::ImportMemoryBufferCollectionFUCHSIA,
                           vk::MemoryDedicatedAllocateInfoKHR>
      alloc_info(vk::MemoryAllocateInfo()
                     .setAllocationSize(memory_requirements.size)
                     .setMemoryTypeIndex(memory_type_index),
                 vk::ImportMemoryBufferCollectionFUCHSIA()
                     .setCollection(collection)
                     .setIndex(metadata.vmo_index),
                 vk::MemoryDedicatedAllocateInfoKHR().setImage(image));
  vk::DeviceMemory memory = nullptr;
  if (const vk::Result err =
          vk_device.allocateMemory(&alloc_info.get<vk::MemoryAllocateInfo>(), nullptr, &memory);
      err != vk::Result::eSuccess) {
    FX_LOGS(ERROR) << "Could not successfully allocate memory: " << vk::to_string(err);
    return nullptr;
  }

  // Have escher manager the memory since this is the required format for creating
  // an Escher image. Also we can now check if the total memory size is great enough
  // for the image memory requirements. If it's not big enough, the client likely
  // requested an image size that is larger than the maximum image size allowed by
  // the sysmem collection constraints.
  const auto gpu_mem =
      escher::GpuMem::AdoptVkMemory(vk_device, vk::DeviceMemory(memory), memory_requirements.size,
                                    /*needs_mapped_ptr*/ false);
  if (memory_requirements.size > gpu_mem->size()) {
    FX_LOGS(ERROR) << "Memory requirements for image exceed available memory: "
                   << memory_requirements.size << " " << gpu_mem->size();
    return nullptr;
  }

  // Create and return an escher image.
  escher::ImageInfo escher_image_info;
  escher_image_info.format = create_info.format;
  escher_image_info.width = create_info.extent.width;
  escher_image_info.height = create_info.extent.height;
  escher_image_info.usage = create_info.usage;
  escher_image_info.memory_flags = readback ? vk::MemoryPropertyFlagBits::eHostCoherent
                                            : vk::MemoryPropertyFlagBits::eDeviceLocal;
  if (create_info.flags & vk::ImageCreateFlagBits::eProtected) {
    escher_image_info.memory_flags = vk::MemoryPropertyFlagBits::eProtected;
  }
  escher_image_info.is_external = true;
  escher_image_info.color_space = escher::FromSysmemColorSpace(
      static_cast<fuchsia::sysmem::ColorSpaceType>(properties.sysmemColorSpaceIndex.colorSpace));
  return escher::impl::NaiveImage::AdoptVkImage(escher_->resource_recycler(), escher_image_info,
                                                image, std::move(gpu_mem),
                                                create_info.initialLayout);
}

escher::TexturePtr VkRenderer::ExtractTexture(const allocation::ImageMetadata& metadata,
                                              vk::BufferCollectionFUCHSIA collection) {
  // Called from main thread or Flatland threads.
  const auto image = ExtractImage(metadata, BufferCollectionUsage::kClientImage, collection,
                                  escher::RectangleCompositor::kTextureUsageFlags);
  if (!image) {
    FX_LOGS(ERROR) << "Image for texture was nullptr.";
    return nullptr;
  }

  escher::SamplerPtr sampler = escher::image_utils::IsYuvFormat(image->format())
                                   ? escher_->sampler_cache()->ObtainYuvSampler(
                                         image->format(), kDefaultFilter, image->color_space())
                                   : escher_->sampler_cache()->ObtainSampler(kDefaultFilter);
  FX_DCHECK(escher::image_utils::IsYuvFormat(image->format()) ? sampler->is_immutable()
                                                              : !sampler->is_immutable());
  return fxl::MakeRefCounted<escher::Texture>(escher_->resource_recycler(), sampler, image);
}

void VkRenderer::Render(const ImageMetadata& render_target,
                        const std::vector<ImageRect>& rectangles,
                        const std::vector<ImageMetadata>& images,
                        const std::vector<zx::event>& release_fences, bool apply_color_conversion) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "VkRenderer::Render");

  FX_DCHECK(rectangles.size() == images.size())
      << "# rects: " << rectangles.size() << " and #images: " << images.size();

  // Copy over the texture and render target data to local containers that do not need
  // to be accessed via a lock. We're just doing a shallow copy via the copy assignment
  // operator since the texture and render target data is just referenced through pointers.
  // We manually unlock the lock after copying over the data.
  lock_.lock();
  const auto local_texture_map = texture_map_;
  const auto local_render_target_map = render_target_map_;
  const auto local_depth_target_map = depth_target_map_;
  const auto local_readback_image_map = readback_image_map_;

  // After moving, the original containers are emptied.
  const auto local_pending_textures = std::move(pending_textures_);
  const auto local_pending_render_targets = std::move(pending_render_targets_);
  pending_textures_.clear();
  pending_render_targets_.clear();
  lock_.unlock();

  // If the |render_target| is protected, we should switch to a protected escher::Frame. Otherwise,
  // we should ensure that there is no protected content in |images|.
  FX_DCHECK(local_render_target_map.find(render_target.identifier) !=
            local_render_target_map.end());
  const bool render_in_protected_mode =
      local_render_target_map.at(render_target.identifier)->use_protected_memory();

  // Escher's frame class acts as a command buffer manager that we use to create a
  // command buffer and submit it to the device queue once we are done.
  const auto frame = escher_->NewFrame(
      "flatland::VkRenderer", ++frame_number_, /*enable_gpu_logging=*/false,
      /*requested_type=*/escher::CommandBuffer::Type::kGraphics, render_in_protected_mode);
  auto command_buffer = frame->cmds();
  if (disable_lazy_pipeline_creation_) {
    command_buffer->DisableLazyPipelineCreation();
  }

  // Transition pending images to their correct layout
  // TODO(https://fxbug.dev/42129471): The way we are transitioning image layouts here and in the
  // rest of Scenic is incorrect for "external" images. It just happens to be working by luck on our
  // current hardware.
  for (auto texture_id : local_pending_textures) {
    FX_DCHECK(local_texture_map.find(texture_id) != local_texture_map.end());
    const auto texture = local_texture_map.at(texture_id);
    command_buffer->impl()->TransitionImageLayout(texture->image(), vk::ImageLayout::eUndefined,
                                                  vk::ImageLayout::eShaderReadOnlyOptimal);
  }
  for (auto target_id : local_pending_render_targets) {
    FX_DCHECK(local_render_target_map.find(target_id) != local_render_target_map.end());
    const auto target = local_render_target_map.at(target_id);
    command_buffer->impl()->TransitionImageLayout(
        target, vk::ImageLayout::eUndefined, vk::ImageLayout::eColorAttachmentOptimal,
        VK_QUEUE_FAMILY_FOREIGN_EXT, escher_->device()->vk_main_queue_family());
  }

  std::vector<escher::TexturePtr> textures;
  std::vector<escher::RectangleCompositor::ColorData> color_data;
  for (const auto& image : images) {
    auto texture_ptr = local_texture_map.at(image.identifier);

    // When we are not in protected mode, replace any protected content with black solid color.
    if (!render_in_protected_mode && texture_ptr->image()->use_protected_memory()) {
      textures.emplace_back(local_texture_map.at(allocation::kInvalidImageId));
      color_data.emplace_back(kProtectedReplacementColorInRGBA, /*is_opaque=*/true);
      continue;
    }

    // Pass the texture into the above vector to keep it alive outside of this loop.
    textures.emplace_back(texture_ptr);

    const glm::vec4 multiply(image.multiply_color[0], image.multiply_color[1],
                             image.multiply_color[2], image.multiply_color[3]);
    color_data.emplace_back(multiply, image.blend_mode == fuchsia_ui_composition::BlendMode::kSrc);
  }

  // Grab the output image and use it to generate a depth texture. The depth texture needs to
  // be the same width and height as the output image.
  const auto output_image = local_render_target_map.at(render_target.identifier);
  const auto depth_texture = local_depth_target_map.at(render_target.identifier);

  // Transition to eColorAttachmentOptimal for rendering.  Note the src queue family is FOREIGN,
  // since we assume that this image was previously presented to the display controller.
  auto render_image_layout = vk::ImageLayout::eColorAttachmentOptimal;
  command_buffer->impl()->TransitionImageLayout(output_image, vk::ImageLayout::eUndefined,
                                                render_image_layout, VK_QUEUE_FAMILY_FOREIGN_EXT,
                                                escher_->device()->vk_main_queue_family());

  const auto normalized_rects = GetNormalizedUvRects(rectangles, images);

  // Now the compositor can finally draw.
  compositor_.DrawBatch(command_buffer, normalized_rects, textures, color_data, output_image,
                        depth_texture, apply_color_conversion);

  const auto readback_image_it = local_readback_image_map.find(render_target.identifier);
  // Copy to the readback image if there is a readback image.
  if (readback_image_it != local_readback_image_map.end()) {
    BlitRenderTarget(command_buffer, output_image, &render_image_layout, readback_image_it->second,
                     render_target);
  }

  // Having drawn, we transition to eGeneral on the FOREIGN target queue, so that we can present the
  // the image to the display controller.
  command_buffer->impl()->TransitionImageLayout(
      output_image, render_image_layout, vk::ImageLayout::eGeneral,
      escher_->device()->vk_main_queue_family(), VK_QUEUE_FAMILY_FOREIGN_EXT);

  // Create vk::semaphores from the zx::events.
  std::vector<escher::SemaphorePtr> semaphores;
  for (auto& fence_original : release_fences) {
    // Since the original fences are passed in by const reference, we
    // duplicate them here so that the duped fences can be moved into
    // the create info struct of the semaphore.
    zx::event fence_copy;
    {
      const auto status = fence_original.duplicate(ZX_RIGHT_SAME_RIGHTS, &fence_copy);
      FX_DCHECK(status == ZX_OK);
    }

    const auto sema = escher::Semaphore::New(escher_->vk_device());
    vk::ImportSemaphoreZirconHandleInfoFUCHSIA info;
    info.semaphore = sema->vk_semaphore();
    info.zirconHandle = fence_copy.release();
    info.handleType = vk::ExternalSemaphoreHandleTypeFlagBits::eZirconEventFUCHSIA;

    {
      TRACE_DURATION("gfx", "VkRenderer::Render[importSemaphoreZirconHandleFUCHSIA]");
      const auto result = escher_->vk_device().importSemaphoreZirconHandleFUCHSIA(
          info, escher_->device()->dispatch_loader());
      FX_DCHECK(result == vk::Result::eSuccess);
    }

    // Create a flow event that ends in the magma system driver.
    if (TRACE_ENABLED()) {
      const zx::event semaphore_event = GetEventForSemaphore(escher_->device(), sema);
      zx_info_handle_basic_t koid_info;
      semaphore_event.get_info(ZX_INFO_HANDLE_BASIC, &koid_info, sizeof(koid_info), nullptr,
                               nullptr);
      TRACE_FLOW_BEGIN("gfx", "semaphore", koid_info.koid);
    }

    semaphores.emplace_back(sema);

    // TODO(https://fxbug.dev/42174813): Semaphore lifetime should be guaranteed by Escher. This
    // wait is a workaround for the issue where we destroy semaphores before they are signalled.
    TRACE_DURATION("gfx", "VkRenderer::Render[WaitOnce]");
    auto wait = std::make_shared<async::WaitOnce>(fence_original.get(), ZX_EVENT_SIGNALED,
                                                  ZX_WAIT_ASYNC_TIMESTAMP);
    zx_status_t wait_status =
        wait->Begin(async_get_default_dispatcher(),
                    [copy_ref = wait, sema](async_dispatcher_t*, async::WaitOnce*, zx_status_t,
                                            const zx_packet_signal_t*) {
                      // Let these fall out of scope.
                    });
  }

  // Submit the commands and wait for them to finish.
  frame->EndFrame(semaphores, nullptr);
}

void VkRenderer::SetColorConversionValues(const fidl::Array<float, 9>& coefficients,
                                          const fidl::Array<float, 3>& preoffsets,
                                          const fidl::Array<float, 3>& postoffsets) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());

  // Coefficients are ordered like this:
  // | c0 c1 c2 0 |
  // | c3 c4 c5 0 |
  // | c6 c7 c8 0 |
  // |  0  0  0 1 |
  //
  // Note: GLM uses column-major memory layout.
  // clang-format off
  const float values[16] = {coefficients[0], coefficients[3], coefficients[6], 0,
                            coefficients[1], coefficients[4], coefficients[7], 0,
                            coefficients[2], coefficients[5], coefficients[8], 0,
                                          0,               0,               0, 1};
  // clang-format on
  const glm::mat4 glm_matrix = glm::make_mat4(values);
  const glm::vec4 glm_preoffsets(preoffsets[0], preoffsets[1], preoffsets[2], 0.0);
  const glm::vec4 glm_postoffsets(postoffsets[0], postoffsets[1], postoffsets[2], 0.0);
  compositor_.SetColorConversionParams({glm_matrix, glm_preoffsets, glm_postoffsets});
}

fuchsia_images2::PixelFormat VkRenderer::ChoosePreferredRenderTargetFormat(
    const std::vector<fuchsia_images2::PixelFormat>& available_formats) const {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());

  for (auto preferred_format : kSupportedRenderTargetImageFormats) {
    for (fuchsia_images2::PixelFormat format : available_formats) {
      const vk::Format vk_format = ConvertToVkFormat(format);
      if (vk_format == preferred_format) {
        return format;
      }
    }
  }
  FX_DCHECK(false) << "Preferred format is not available.";
  return fuchsia_images2::PixelFormat::kInvalid;
}

bool VkRenderer::SupportsRenderInProtected() const {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());

  return escher_->allow_protected_memory();
}

bool VkRenderer::RequiresRenderInProtected(
    const std::vector<allocation::ImageMetadata>& images) const {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  std::scoped_lock lock(lock_);

  for (const auto& image : images) {
    FX_DCHECK(texture_map_.find(image.identifier) != texture_map_.end());
    if (texture_map_.at(image.identifier)->image()->use_protected_memory()) {
      return true;
    }
  }
  return false;
}

bool VkRenderer::WaitIdle() {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());

  return escher_->vk_device().waitIdle() == vk::Result::eSuccess;
}

void VkRenderer::WarmPipelineCache() {
  TRACE_DURATION("gfx", "VkRenderer::WarmPipelineCache");

  for (auto output_format : kSupportedRenderTargetImageFormats) {
    auto depth_format = escher_->device()->caps().GetMatchingDepthStencilFormat().value;

    auto immutable_samplers = utils::ImmutableSamplersForShaderWarmup(escher_, kDefaultFilter);

    // Depending on the memory types provided by the Vulkan implementation, separate versions of the
    // render-passes (and therefore pipelines) may be required for protected/non-protected memory.
    // Or not; if not, then the second call will simply use the ones that are already cached.
    compositor_.WarmPipelineCache(output_format, vk::ImageLayout::eColorAttachmentOptimal,
                                  depth_format, immutable_samplers,
                                  /* use_protected_memory= */ true);
    compositor_.WarmPipelineCache(output_format, vk::ImageLayout::eColorAttachmentOptimal,
                                  depth_format, immutable_samplers,
                                  /* use_protected_memory= */ false);
  }
}

void VkRenderer::BlitRenderTarget(escher::CommandBuffer* command_buffer,
                                  const escher::ImagePtr source_image,
                                  vk::ImageLayout* source_image_layout,
                                  const escher::ImagePtr dest_image,
                                  const ImageMetadata& metadata) {
  FX_DCHECK(main_dispatcher_ == async_get_default_dispatcher());
  TRACE_DURATION("gfx", "VkRenderer::BlitRenderTarget");

  command_buffer->TransitionImageLayout(source_image, *source_image_layout,
                                        vk::ImageLayout::eTransferSrcOptimal);
  *source_image_layout = vk::ImageLayout::eTransferSrcOptimal;
  command_buffer->TransitionImageLayout(dest_image, vk::ImageLayout::eUndefined,
                                        vk::ImageLayout::eTransferDstOptimal);
  command_buffer->Blit(
      source_image, vk::Offset2D(0, 0), vk::Extent2D(metadata.width, metadata.height), dest_image,
      vk::Offset2D(0, 0), vk::Extent2D(metadata.width, metadata.height), kDefaultFilter);
}

std::unordered_map<GlobalBufferCollectionId, VkRenderer::CollectionData>&
VkRenderer::GetBufferCollectionsFor(const BufferCollectionUsage usage) {
  // Called from main thread or Flatland threads.
  switch (usage) {
    case BufferCollectionUsage::kRenderTarget:
      return render_target_collections_;
    case BufferCollectionUsage::kReadback:
      return readback_collections_;
    case BufferCollectionUsage::kClientImage:
      return texture_collections_;
    default:
      FX_NOTREACHED();
      return texture_collections_;
  }
}

}  // namespace flatland
