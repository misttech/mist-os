// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/renderer/null_renderer.h"

#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <optional>

namespace flatland {

bool NullRenderer::ImportBufferCollection(
    allocation::GlobalBufferCollectionId collection_id,
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
    BufferCollectionUsage usage, std::optional<fuchsia::math::SizeU> size) {
  FX_DCHECK(collection_id != allocation::kInvalidId);
  FX_DCHECK(token.is_valid());

  std::scoped_lock lock(lock_);
  auto& map = GetBufferCollectionInfosFor(usage);
  if (map.find(collection_id) != map.end()) {
    FX_LOGS(ERROR) << "Duplicate GlobalBufferCollectionID: " << collection_id;
    return false;
  }
  std::optional<fuchsia::sysmem2::ImageFormatConstraints> image_constraints;
  if (size.has_value()) {
    image_constraints = std::make_optional<fuchsia::sysmem2::ImageFormatConstraints>();
    image_constraints->set_pixel_format(fuchsia::images2::PixelFormat::B8G8R8A8);
    image_constraints->mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
    image_constraints->set_required_min_size(
        fuchsia::math::SizeU{.width = size->width, .height = size->height});
    image_constraints->set_required_max_size(
        fuchsia::math::SizeU{.width = size->width, .height = size->height});
  }
  fuchsia::sysmem2::BufferUsage sysmem_usage;
  sysmem_usage.set_none(fuchsia::sysmem2::NONE_USAGE);
  auto result =
      BufferCollectionInfo::New(sysmem_allocator, std::move(token), std::move(image_constraints),
                                std::move(sysmem_usage), usage);
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Unable to register collection.";
    return false;
  }

  // Multiple threads may be attempting to read/write from |map| so we
  // lock this function here.
  // TODO(https://fxbug.dev/42120738): Convert this to a lock-free structure.
  map[collection_id] = std::move(result.value());
  return true;
}

void NullRenderer::ReleaseBufferCollection(allocation::GlobalBufferCollectionId collection_id,
                                           BufferCollectionUsage usage) {
  // Multiple threads may be attempting to read/write from the various maps,
  // lock this function here.
  // TODO(https://fxbug.dev/42120738): Convert this to a lock-free structure.
  std::scoped_lock lock(lock_);
  auto& map = GetBufferCollectionInfosFor(usage);

  auto collection_itr = map.find(collection_id);

  // If the collection is not in the map, then there's nothing to do.
  if (collection_itr == map.end()) {
    return;
  }

  // Erase the sysmem collection from the map.
  map.erase(collection_id);
}

bool NullRenderer::ImportBufferImage(const allocation::ImageMetadata& metadata,
                                     BufferCollectionUsage usage) {
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

  std::scoped_lock lock(lock_);
  auto& map = GetBufferCollectionInfosFor(usage);

  const auto& collection_itr = map.find(metadata.collection_id);
  if (collection_itr == map.end()) {
    FX_LOGS(ERROR) << "Collection with id " << metadata.collection_id << " does not exist.";
    return false;
  }

  auto& collection = collection_itr->second;
  if (!collection.BuffersAreAllocated()) {
    FX_LOGS(ERROR) << "Buffers for collection " << metadata.collection_id
                   << " have not been allocated.";
    return false;
  }

  const auto& sysmem_info = collection.GetSysmemInfo();
  const auto vmo_count = sysmem_info.buffers().size();
  const auto& image_constraints = sysmem_info.settings().image_format_constraints();

  if (metadata.vmo_index >= vmo_count) {
    FX_LOGS(ERROR) << "ImportBufferImage failed, vmo_index " << metadata.vmo_index
                   << " must be less than vmo_count " << vmo_count;
    return false;
  }

  if (metadata.width < image_constraints.min_size().width ||
      metadata.width > image_constraints.max_size().width) {
    FX_LOGS(ERROR) << "ImportBufferImage failed, width " << metadata.width
                   << " is not within valid range [" << image_constraints.min_size().width << ","
                   << image_constraints.max_size().width << "]";
    return false;
  }

  if (metadata.height < image_constraints.min_size().height ||
      metadata.height > image_constraints.max_size().height) {
    FX_LOGS(ERROR) << "ImportBufferImage failed, height " << metadata.height
                   << " is not within valid range [" << image_constraints.min_size().height << ","
                   << image_constraints.max_size().height << "]";
    return false;
  }

  if (usage == BufferCollectionUsage::kClientImage) {
    image_map_[metadata.identifier] = fidl::Clone(image_constraints);
  }
  return true;
}

void NullRenderer::ReleaseBufferImage(allocation::GlobalImageId image_id) {
  FX_DCHECK(image_id != allocation::kInvalidImageId);
  std::scoped_lock lock(lock_);
  image_map_.erase(image_id);
}

void NullRenderer::SetColorConversionValues(const std::array<float, 9>& coefficients,
                                            const std::array<float, 3>& preoffsets,
                                            const std::array<float, 3>& postoffsets) {}

// Check that the buffer collections for each of the images passed in have been validated.
// DCHECK if they have not.
void NullRenderer::Render(const allocation::ImageMetadata& render_target,
                          const std::vector<ImageRect>& rectangles,
                          const std::vector<allocation::ImageMetadata>& images,
                          const std::vector<zx::event>& release_fences,
                          bool apply_color_conversion) {
  std::scoped_lock lock(lock_);
  for (const auto& image : images) {
    auto image_id = image.identifier;
    FX_DCHECK(image_id != allocation::kInvalidId);

    const auto& image_map_itr_ = image_map_.find(image_id);
    FX_DCHECK(image_map_itr_ != image_map_.end());
    const auto& image_constraints = image_map_itr_->second;

    // Make sure the image conforms to the constraints of the collection.
    FX_DCHECK(image.width <= image_constraints.max_size().width);
    FX_DCHECK(image.height <= image_constraints.max_size().height);
  }

  // Fire all of the release fences.
  for (auto& fence : release_fences) {
    fence.signal(0, ZX_EVENT_SIGNALED);
  }
}

fuchsia_images2::PixelFormat NullRenderer::ChoosePreferredRenderTargetFormat(
    const std::vector<fuchsia_images2::PixelFormat>& available_formats) const {
  for (const auto& format : available_formats) {
    if (format == fuchsia_images2::PixelFormat::kB8G8R8A8) {
      return format;
    }
  }
  FX_DCHECK(false) << "Preferred format is not available.";
  return fuchsia_images2::PixelFormat::kInvalid;
}

bool NullRenderer::SupportsRenderInProtected() const { return false; }

bool NullRenderer::RequiresRenderInProtected(
    const std::vector<allocation::ImageMetadata>& images) const {
  return false;
}

std::unordered_map<allocation::GlobalBufferCollectionId, BufferCollectionInfo>&
NullRenderer::GetBufferCollectionInfosFor(BufferCollectionUsage usage) {
  switch (usage) {
    case BufferCollectionUsage::kRenderTarget:
      return render_target_map_;
    case BufferCollectionUsage::kReadback:
      return readback_map_;
    case BufferCollectionUsage::kClientImage:
      return client_image_map_;
    default:
      FX_NOTREACHED();
      return render_target_map_;
  }
}

}  // namespace flatland
