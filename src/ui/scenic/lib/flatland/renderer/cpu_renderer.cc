// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/renderer/cpu_renderer.h"

#include <fuchsia/images2/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/sysmem2/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include <cmath>
#include <cstdint>
#include <optional>

#include "src/ui/scenic/lib/allocation/id.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/flatland/flatland_types.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/lib/utils/pixel.h"

namespace flatland {

const std::vector<uint8_t> kTransparent = {0, 0, 0, 0};
constexpr uint32_t kAlphaIndex = 3;

bool CpuRenderer::ImportBufferCollection(
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
    image_constraints->set_pixel_format_modifier(fuchsia::images2::PixelFormatModifier::LINEAR);
    image_constraints->mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
    image_constraints->set_required_min_size(
        fuchsia::math::SizeU{.width = size->width, .height = size->height});
    image_constraints->set_required_max_size(
        fuchsia::math::SizeU{.width = size->width, .height = size->height});
  }
  fuchsia::sysmem2::BufferUsage buffer_usage;
  buffer_usage.set_cpu(fuchsia::sysmem2::CPU_USAGE_READ);
  auto result =
      BufferCollectionInfo::New(sysmem_allocator, std::move(token), std::move(image_constraints),
                                std::move(buffer_usage), usage);
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
  const auto vmo_count = sysmem_info.buffers().size();
  auto& image_constraints = sysmem_info.settings().image_format_constraints();

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

  const zx::vmo& vmo = sysmem_info.buffers()[metadata.vmo_index].vmo();
  zx::vmo dup;
  zx_status_t status = vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
  FX_DCHECK(status == ZX_OK);
  image_map_[metadata.identifier] = std::make_pair(std::move(dup), fidl::Clone(image_constraints));

  return true;
}

void CpuRenderer::ReleaseBufferImage(allocation::GlobalImageId image_id) {
  FX_DCHECK(image_id != allocation::kInvalidImageId);
  std::scoped_lock lock(lock_);
  image_map_.erase(image_id);
}

void CpuRenderer::SetColorConversionValues(const fidl::Array<float, 9>& coefficients,
                                           const fidl::Array<float, 3>& preoffsets,
                                           const fidl::Array<float, 3>& postoffsets) {}

void CpuRenderer::Render(const allocation::ImageMetadata& render_target,
                         const std::vector<ImageRect>& rectangles,
                         const std::vector<allocation::ImageMetadata>& images,
                         const std::vector<zx::event>& release_fences = {},
                         bool apply_color_conversion = false) {
  TRACE_DURATION("gfx", "CpuRenderer::Render");
  std::scoped_lock lock(lock_);
  FX_CHECK(images.size() == rectangles.size());
  for (uint32_t i = 0; i < images.size(); i++) {
    // TODO(b/304596608): Currently this solution is not feature complete:
    //   * We let multiple images overwrite each other.
    //   * Origin is ignored.
    //   * Mapping to the target rectangle is ignored.
    // Fix these use cases.
    auto image = images[i];
    auto image_id = image.identifier;

    ImageRect rectangle = rectangles[i];
    uint32_t rectangle_width = static_cast<uint32_t>(rectangle.extent.x);
    uint32_t rectangle_height = static_cast<uint32_t>(rectangle.extent.y);

    constexpr uint64_t kBytesPerPixel = 4;

    // |allocation::kInvalidImageId| indicates solid fill color. Create and fill vmo for common ops.
    if (image_id == allocation::kInvalidImageId) {
      image.width = rectangle_width;
      image.height = rectangle_height;
      fuchsia::sysmem2::ImageFormatConstraints constraints;
      *constraints.mutable_pixel_format() = fuchsia::images2::PixelFormat::R8G8B8A8;
      *constraints.mutable_max_size() = {.width = rectangle_width, .height = rectangle_height};
      *constraints.mutable_bytes_per_row_divisor() = 1;
      *constraints.mutable_min_bytes_per_row() = rectangle_width * kBytesPerPixel;
      zx::vmo solid_fill_vmo;
      zx::vmo::create(kBytesPerPixel * rectangle_width * rectangle_height, 0, &solid_fill_vmo);
      MapHostPointer(solid_fill_vmo, flatland::HostPointerAccessMode::kWriteOnly,
                     [&image](uint8_t* vmo_ptr, uint32_t num_bytes) {
                       for (uint32_t i = 0; i < num_bytes; ++i) {
                         vmo_ptr[i] =
                             static_cast<uint8_t>(255 * image.multiply_color[i % kBytesPerPixel]);
                       }
                     });
      image_map_[allocation::kInvalidImageId] =
          std::make_pair(std::move(solid_fill_vmo), std::move(constraints));
    }

    const auto& image_map_itr_ = image_map_.find(image_id);
    // Dereferencing the end marker later is UB, so better fail right here.
    FX_CHECK(image_map_itr_ != image_map_.end()) << "not found image_id: " << image_id;
    const auto& image_constraints = image_map_itr_->second.second;

    // Make sure the image conforms to the constraints of the collection.
    FX_CHECK(image.width <= image_constraints.max_size().width);
    FX_CHECK(image.height <= image_constraints.max_size().height);

    auto render_target_id = render_target.identifier;
    FX_CHECK(render_target_id != allocation::kInvalidId);
    const auto& render_target_map_itr_ = image_map_.find(render_target_id);
    FX_CHECK(render_target_map_itr_ != image_map_.end());
    const auto& render_target_constraints = render_target_map_itr_->second.second;

    fuchsia::images2::PixelFormat image_type = image_constraints.pixel_format();
    fuchsia::images2::PixelFormat render_type = render_target_constraints.pixel_format();
    FX_CHECK(utils::Pixel::IsFormatSupported(image_type));
    FX_CHECK(utils::Pixel::IsFormatSupported(render_type));

    // The rectangle, image, and render_target should be compatible, e.g. the image dimensions are
    // equal to the rectangle dimensions and less than or equal to the render target dimensions.
    FX_CHECK(rectangle.orientation == fuchsia::ui::composition::Orientation::CCW_0_DEGREES);
    FX_CHECK(rectangle_width <= render_target.width);
    FX_CHECK(rectangle_height <= render_target.height);

    auto image_pixels_per_row = utils::GetPixelsPerRow(image_constraints, image.width);
    auto render_target_pixels_per_row =
        utils::GetPixelsPerRow(render_target_constraints, render_target.width);

    // Ensure that the inner loop below is using the correct size pixel format.
    FX_CHECK(utils::Pixel(255, 255, 255, 255).ToFormat(render_type).size() == kBytesPerPixel);

    // Copy the image vmo into the render_target vmo.
    MapHostPointer(
        render_target_map_itr_->second.first, HostPointerAccessMode::kWriteOnly,
        [&image_map_itr_, image, render_target, image_pixels_per_row, render_target_pixels_per_row,
         image_type, render_type](uint8_t* render_target_ptr, uint32_t render_target_num_bytes) {
          MapHostPointer(
              image_map_itr_->second.first, HostPointerAccessMode::kReadOnly,
              [render_target_ptr, image, render_target, image_pixels_per_row,
               render_target_pixels_per_row, image_type,
               render_type](const uint8_t* image_ptr, uint32_t image_num_bytes) {
                uint32_t min_height = std::min(image.height, render_target.height);
                uint32_t min_width = std::min(image.width, render_target.width);

                // Avoid allocation in the inner loop.
                std::vector<uint8_t> color;
                color.reserve(kBytesPerPixel);
                for (uint32_t y = 0; y < min_height; y++) {
                  // Copy image pixels into the render target.
                  //
                  // Note that the stride of the buffer may be different
                  // than the width of the image due to memory alignment, so we use the
                  // pixels per row instead.
                  for (uint32_t x = 0; x < min_width; x++) {
                    utils::Pixel pixel =
                        utils::Pixel::FromVmo(image_ptr, image_pixels_per_row, x, y, image_type);
                    pixel.ToFormat(render_type, color);
                    uint32_t start =
                        y * render_target_pixels_per_row * kBytesPerPixel + x * kBytesPerPixel;
                    for (uint32_t offset = 0; offset < kBytesPerPixel; offset++) {
                      const uint32_t index = start + offset;
                      switch (image.blend_mode) {
                        case fuchsia_ui_composition::BlendMode::kSrc:
                          render_target_ptr[index] = color[offset];
                          break;
                        case fuchsia_ui_composition::BlendMode::kSrcOver:
                          render_target_ptr[index] =
                              color[offset] +
                              static_cast<uint8_t>((1.f - image.multiply_color[kAlphaIndex]) *
                                                   render_target_ptr[index]);
                          break;
                      }
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

fuchsia_images2::PixelFormat CpuRenderer::ChoosePreferredRenderTargetFormat(
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
