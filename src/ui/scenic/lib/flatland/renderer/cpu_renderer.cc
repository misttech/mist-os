// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/renderer/cpu_renderer.h"

#include <lib/syslog/cpp/macros.h>

#include <cmath>
#include <cstdint>
#include <optional>

#include "fuchsia/sysmem/cpp/fidl.h"
#include "fuchsia/ui/composition/cpp/fidl.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/flatland/flatland_types.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/lib/utils/pixel.h"

namespace flatland {

const std::vector<uint8_t> kTransparent = {0, 0, 0, 0};

bool CpuRenderer::ImportBufferCollection(
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
  image_constraints->pixel_format.format_modifier.value = fuchsia::sysmem::FORMAT_MODIFIER_LINEAR;
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
  auto result = BufferCollectionInfo::New(
      sysmem_allocator, std::move(token), image_constraints,
      fuchsia::sysmem::BufferUsage{.cpu = fuchsia::sysmem::cpuUsageRead}, usage);
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

void CpuRenderer::ReleaseBufferCollection(allocation::GlobalBufferCollectionId collection_id,
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

bool CpuRenderer::ImportBufferImage(const allocation::ImageMetadata& metadata,
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

  const zx::vmo& vmo = sysmem_info.buffers[metadata.vmo_index].vmo;
  zx::vmo dup;
  zx_status_t status = vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
  FX_DCHECK(status == ZX_OK);
  image_map_[metadata.identifier] = std::make_pair(std::move(dup), image_constraints);

  return true;
}

void CpuRenderer::ReleaseBufferImage(allocation::GlobalImageId image_id) {
  FX_DCHECK(image_id != allocation::kInvalidImageId);
  std::scoped_lock lock(lock_);
  image_map_.erase(image_id);
}

void CpuRenderer::SetColorConversionValues(const std::array<float, 9>& coefficients,
                                           const std::array<float, 3>& preoffsets,
                                           const std::array<float, 3>& postoffsets) {}

void CpuRenderer::Render(const allocation::ImageMetadata& render_target,
                         const std::vector<ImageRect>& rectangles,
                         const std::vector<allocation::ImageMetadata>& images,
                         const std::vector<zx::event>& release_fences = {},
                         bool apply_color_conversion = false) {
  std::scoped_lock lock(lock_);
  FX_DCHECK(images.size() == rectangles.size());
  if (images.size() == 1) {
    // TODO(b/304596608): Handle the most basic case first (images[0]).
    // This should be extended to other cases later.
    auto image = images[0];
    auto image_id = image.identifier;
    FX_DCHECK(image_id != allocation::kInvalidId);

    const auto& image_map_itr_ = image_map_.find(image_id);
    FX_DCHECK(image_map_itr_ != image_map_.end());
    auto image_constraints = image_map_itr_->second.second;

    // Make sure the image conforms to the constraints of the collection.
    FX_DCHECK(image.width <= image_constraints.max_coded_width);
    FX_DCHECK(image.height <= image_constraints.max_coded_height);

    auto render_target_id = render_target.identifier;
    FX_DCHECK(render_target_id != allocation::kInvalidId);
    const auto& render_target_map_itr_ = image_map_.find(render_target_id);
    FX_DCHECK(render_target_map_itr_ != image_map_.end());
    auto render_target_constraints = render_target_map_itr_->second.second;

    // The image and render_target should be BGRA32 or R8G8B8A8.
    fuchsia::sysmem::PixelFormatType image_type = image_constraints.pixel_format.type;
    fuchsia::sysmem::PixelFormatType render_type = render_target_constraints.pixel_format.type;
    FX_DCHECK(utils::Pixel::IsFormatSupported(image_type));
    FX_DCHECK(utils::Pixel::IsFormatSupported(render_type));

    // The rectangle, image, and render_target should be compatible, e.g. the image dimensions are
    // equal to the rectangle dimensions and less than or equal to the render target dimensions.
    ImageRect rectangle = rectangles[0];
    FX_DCHECK(rectangle.orientation == fuchsia::ui::composition::Orientation::CCW_0_DEGREES);
    uint32_t rectangle_width = static_cast<uint32_t>(rectangle.extent.x);
    uint32_t rectangle_height = static_cast<uint32_t>(rectangle.extent.y);
    FX_DCHECK(rectangle_width <= render_target.width);
    FX_DCHECK(rectangle_height <= render_target.height);
    FX_DCHECK(rectangle_width == image.width);
    FX_DCHECK(rectangle_height == image.height);
    FX_DCHECK(rectangle.origin.x == 0);
    FX_DCHECK(rectangle.origin.y == 0);

    auto image_pixels_per_row = utils::GetPixelsPerRow(image_constraints, image.width);
    auto render_target_pixels_per_row =
        utils::GetPixelsPerRow(render_target_constraints, render_target.width);

    // Copy the image vmo into the render_target vmo.
    MapHostPointer(
        render_target_map_itr_->second.first, HostPointerAccessMode::kWriteOnly,
        [&image_map_itr_, image, render_target, image_pixels_per_row, render_target_pixels_per_row,
         image_type, render_type](uint8_t* render_target_ptr, uint32_t render_target_num_bytes) {
          MapHostPointer(image_map_itr_->second.first, HostPointerAccessMode::kReadOnly,
                         [render_target_ptr, render_target_num_bytes, image, render_target,
                          image_pixels_per_row, render_target_pixels_per_row, image_type,
                          render_type](const uint8_t* image_ptr, uint32_t image_num_bytes) {
                           FX_DCHECK(image_num_bytes <= render_target_num_bytes);
                           uint32_t min_height = std::min(image.height, render_target.height);
                           uint32_t min_width = std::min(image.width, render_target.width);
                           for (uint32_t y = 0; y < min_height; y++) {
                             // Copy image pixels into the render target.
                             //
                             // Note that the stride of the buffer may be different
                             // than the width of the image due to memory alignment, so we use the
                             // pixels per row instead.
                             for (uint32_t x = 0; x < min_width; x++) {
                               utils::Pixel pixel = utils::Pixel::FromVmo(
                                   image_ptr, image_pixels_per_row, x, y, image_type);
                               std::vector<uint8_t> color = pixel.ToFormat(render_type);
                               uint32_t start = y * render_target_pixels_per_row * 4 + x * 4;
                               for (uint32_t offset = 0; offset < 4; offset++) {
                                 render_target_ptr[start + offset] = color[offset];
                               }
                             }
                           }
                         });
        });
  }

  // Fire all of the release fences.
  for (auto& fence : release_fences) {
    fence.signal(0, ZX_EVENT_SIGNALED);
  }
}

fuchsia_images2::PixelFormat CpuRenderer::ChoosePreferredPixelFormat(
    const std::vector<fuchsia_images2::PixelFormat>& available_formats) const {
  for (const auto& format : available_formats) {
    if (format == fuchsia_images2::PixelFormat::kB8G8R8A8) {
      return format;
    }
  }
  FX_DCHECK(false) << "Preferred format is not available.";
  return fuchsia_images2::PixelFormat::kInvalid;
}

bool CpuRenderer::SupportsRenderInProtected() const { return false; }

bool CpuRenderer::RequiresRenderInProtected(
    const std::vector<allocation::ImageMetadata>& images) const {
  return false;
}

std::unordered_map<allocation::GlobalBufferCollectionId, BufferCollectionInfo>&
CpuRenderer::GetBufferCollectionInfosFor(BufferCollectionUsage usage) {
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
