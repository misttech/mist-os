// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/renderer/null_renderer.h"

#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <optional>

#include "fidl/fuchsia.images2/cpp/wire_types.h"

namespace flatland {

bool NullRenderer::ImportBufferCollection(
    allocation::GlobalBufferCollectionId collection_id,
    fuchsia::sysmem::Allocator_Sync* sysmem_allocator,
    fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
    BufferCollectionUsage usage, std::optional<fuchsia::math::SizeU> size) {
  FX_DCHECK(collection_id != allocation::kInvalidId);
  FX_DCHECK(token.is_valid());

  std::scoped_lock lock(lock_);
  auto& map = GetBufferCollectionInfosFor(usage);
  if (map.find(collection_id) != map.end()) {
    FX_LOGS(ERROR) << "Duplicate GlobalBufferCollectionID: " << collection_id;
    return false;
  }
  std::optional<fuchsia::sysmem::ImageFormatConstraints> image_constraints;
  if (size.has_value()) {
    image_constraints = std::make_optional<fuchsia::sysmem::ImageFormatConstraints>();
    image_constraints->pixel_format.type = fuchsia::sysmem::PixelFormatType::BGRA32;
    image_constraints->color_spaces_count = 1;
    image_constraints->color_space[0] =
        fuchsia::sysmem::ColorSpace{.type = fuchsia::sysmem::ColorSpaceType::SRGB};
    image_constraints->required_min_coded_width = size->width;
    image_constraints->required_min_coded_height = size->height;
    image_constraints->required_max_coded_width = size->width;
    image_constraints->required_max_coded_height = size->height;
  }
  auto result =
      BufferCollectionInfo::New(sysmem_allocator, std::move(token), std::move(image_constraints));
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Unable to register collection.";
    return false;
  }

  // Multiple threads may be attempting to read/write from |map| so we
  // lock this function here.
  // TODO(fxbug.dev/44335): Convert this to a lock-free structure.
  map[collection_id] = std::move(result.value());
  return true;
}

void NullRenderer::ReleaseBufferCollection(allocation::GlobalBufferCollectionId collection_id,
                                           BufferCollectionUsage usage) {
  // Multiple threads may be attempting to read/write from the various maps,
  // lock this function here.
  // TODO(fxbug.dev/44335): Convert this to a lock-free structure.
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
  const auto vmo_count = sysmem_info.buffer_count;
  auto image_constraints = sysmem_info.settings.image_format_constraints;

  if (metadata.vmo_index >= vmo_count) {
    FX_LOGS(ERROR) << "ImportBufferImage failed, vmo_index " << metadata.vmo_index
                   << " must be less than vmo_count " << vmo_count;
    return false;
  }

  if (metadata.width < image_constraints.min_coded_width ||
      metadata.width > image_constraints.max_coded_width) {
    FX_LOGS(ERROR) << "ImportBufferImage failed, width " << metadata.width
                   << " is not within valid range [" << image_constraints.min_coded_width << ","
                   << image_constraints.max_coded_width << "]";
    return false;
  }

  if (metadata.height < image_constraints.min_coded_height ||
      metadata.height > image_constraints.max_coded_height) {
    FX_LOGS(ERROR) << "ImportBufferImage failed, height " << metadata.height
                   << " is not within valid range [" << image_constraints.min_coded_height << ","
                   << image_constraints.max_coded_height << "]";
    return false;
  }

  if (usage == BufferCollectionUsage::kClientImage) {
    image_map_[metadata.identifier] = image_constraints;
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
    auto image_constraints = image_map_itr_->second;

    // Make sure the image conforms to the constraints of the collection.
    FX_DCHECK(image.width <= image_constraints.max_coded_width);
    FX_DCHECK(image.height <= image_constraints.max_coded_height);
  }

  // Fire all of the release fences.
  for (auto& fence : release_fences) {
    fence.signal(0, ZX_EVENT_SIGNALED);
  }
}

fuchsia_images2::PixelFormat NullRenderer::ChoosePreferredPixelFormat(
    const std::vector<fuchsia_images2::PixelFormat>& available_formats) const {
  for (const auto& format : available_formats) {
    if (format == fuchsia_images2::PixelFormat::kBgra32) {
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
