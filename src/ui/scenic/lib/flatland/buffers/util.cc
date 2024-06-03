// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/buffers/util.h"

namespace flatland {

fuchsia::sysmem2::BufferUsage get_none_usage() {
  static fuchsia::sysmem2::BufferUsage none_usage = [] {
    fuchsia::sysmem2::BufferUsage result;
    result.set_none(fuchsia::sysmem2::NONE_USAGE);
    return result;
  }();
  return fidl::Clone(none_usage);
}

const std::pair<fuchsia::sysmem2::BufferUsage, fuchsia::sysmem2::BufferMemoryConstraints>
GetUsageAndMemoryConstraintsForCpuWriteOften() {
  static const fuchsia::sysmem2::BufferMemoryConstraints kCpuConstraints = [] {
    fuchsia::sysmem2::BufferMemoryConstraints bmc;
    bmc.set_ram_domain_supported(true);
    bmc.set_cpu_domain_supported(true);
    return bmc;
  }();
  static const fuchsia::sysmem2::BufferUsage kCpuWriteUsage = [] {
    fuchsia::sysmem2::BufferUsage usage;
    usage.set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE_OFTEN);
    return usage;
  }();
  return std::make_pair(fidl::Clone(kCpuWriteUsage), fidl::Clone(kCpuConstraints));
}

void SetClientConstraintsAndWaitForAllocated(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token, uint32_t image_count, uint32_t width,
    uint32_t height, fuchsia::sysmem2::BufferUsage usage,
    const std::vector<fuchsia::images2::PixelFormatModifier>& additional_format_modifiers,
    std::optional<fuchsia::sysmem2::BufferMemoryConstraints> memory_constraints) {
  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(token));
  bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
  zx_status_t status = sysmem_allocator->BindSharedCollection(std::move(bind_shared_request));
  FX_DCHECK(status == ZX_OK);

  // Use a name with a priority thats > the vulkan implementation, but < what any client would use.
  fuchsia::sysmem2::NodeSetNameRequest set_name_request;
  set_name_request.set_priority(10u);
  set_name_request.set_name("FlatlandImage");
  buffer_collection->SetName(std::move(set_name_request));

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  auto& constraints = *set_constraints_request.mutable_constraints();

  if (memory_constraints) {
    constraints.set_buffer_memory_constraints(std::move(*memory_constraints));
  }
  constraints.set_usage(std::move(usage));
  constraints.set_min_buffer_count(image_count);

  size_t image_format_constraints_count =
      1 + static_cast<uint32_t>(additional_format_modifiers.size());
  for (size_t i = 0; i < image_format_constraints_count; i++) {
    auto& image_constraints = constraints.mutable_image_format_constraints()->emplace_back();
    image_constraints.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
    image_constraints.set_pixel_format(fuchsia::images2::PixelFormat::R8G8B8A8);
    image_constraints.set_pixel_format_modifier(i == 0
                                                    ? fuchsia::images2::PixelFormatModifier::LINEAR
                                                    : additional_format_modifiers[i - 1]);
    image_constraints.set_required_min_size(fuchsia::math::SizeU{.width = width, .height = height});
    image_constraints.set_required_max_size(fuchsia::math::SizeU{.width = width, .height = height});
    image_constraints.set_max_size(
        fuchsia::math::SizeU{.width = width * 4 /*num channels*/, .height = height});
    image_constraints.set_max_bytes_per_row(0xffffffff);
  }

  status = buffer_collection->SetConstraints(std::move(set_constraints_request));
  FX_DCHECK(status == ZX_OK);

  // Have the client wait for allocation.
  fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
  status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
  FX_DCHECK(status == ZX_OK);
  FX_DCHECK(!wait_result.is_framework_err());
  FX_DCHECK(!wait_result.is_err());
  FX_DCHECK(wait_result.is_response());

  status = buffer_collection->Release();
  FX_DCHECK(status == ZX_OK);
}

fuchsia::sysmem2::BufferCollectionSyncPtr CreateBufferCollectionSyncPtrAndSetConstraints(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token, uint32_t image_count, uint32_t width,
    uint32_t height, fuchsia::sysmem2::BufferUsage usage, fuchsia::images2::PixelFormat format,
    std::optional<fuchsia::sysmem2::BufferMemoryConstraints> memory_constraints,
    std::optional<fuchsia::images2::PixelFormatModifier> pixel_format_modifier) {
  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(token));
  bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
  zx_status_t status = sysmem_allocator->BindSharedCollection(std::move(bind_shared_request));
  FX_DCHECK(status == ZX_OK);

  // Use a name with a priority thats > the vulkan implementation, but < what any client would use.
  fuchsia::sysmem2::NodeSetNameRequest set_name_request;
  set_name_request.set_priority(10u);
  set_name_request.set_name("FlatlandClientPointer");
  buffer_collection->SetName(std::move(set_name_request));

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  auto& constraints = *set_constraints_request.mutable_constraints();
  if (memory_constraints) {
    constraints.set_buffer_memory_constraints(std::move(*memory_constraints));
  }

  constraints.set_usage(std::move(usage));
  constraints.set_min_buffer_count(image_count);

  auto& image_constraints = constraints.mutable_image_format_constraints()->emplace_back();

  image_constraints.set_pixel_format(format);

  if (pixel_format_modifier.has_value()) {
    image_constraints.set_pixel_format_modifier(*pixel_format_modifier);
  }

  switch (format) {
    case fuchsia::images2::PixelFormat::B8G8R8A8:
    case fuchsia::images2::PixelFormat::R8G8B8A8:
      image_constraints.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
      break;
    case fuchsia::images2::PixelFormat::I420:
    case fuchsia::images2::PixelFormat::NV12:
      image_constraints.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::REC709);
      break;
    default:
      FX_NOTREACHED();
  }

  image_constraints.set_required_min_size(fuchsia::math::SizeU{.width = width, .height = height});
  image_constraints.set_required_max_size(fuchsia::math::SizeU{.width = width, .height = height});
  image_constraints.set_max_size(fuchsia::math::SizeU{.width = width * 4, .height = height});
  image_constraints.set_max_bytes_per_row(0xffffffff);

  status = buffer_collection->SetConstraints(std::move(set_constraints_request));
  FX_DCHECK(status == ZX_OK);

  return buffer_collection;
}

namespace {

zx_vm_option_t HostPointerAccessModeToVmoOptions(HostPointerAccessMode host_pointer_access_mode) {
  switch (host_pointer_access_mode) {
    case HostPointerAccessMode::kReadOnly:
      return ZX_VM_PERM_READ;
    case HostPointerAccessMode::kWriteOnly:
    case HostPointerAccessMode::kReadWrite:
      return ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;
    default:
      ZX_ASSERT_MSG(false, "Invalid HostPointerAccessMode %u",
                    static_cast<unsigned int>(host_pointer_access_mode));
  }
}

}  // namespace

void MapHostPointer(const fuchsia::sysmem2::BufferCollectionInfo& collection_info, uint32_t vmo_idx,
                    HostPointerAccessMode host_pointer_access_mode,
                    std::function<void(uint8_t*, uint32_t)> callback) {
  // If the vmo idx is out of bounds pass in a nullptr and 0 bytes back to the caller.
  if (vmo_idx >= collection_info.buffers().size()) {
    callback(nullptr, 0);
    return;
  }

  auto vmo_bytes = collection_info.settings().buffer_settings().size_bytes();
  FX_DCHECK(vmo_bytes > 0);

  MapHostPointer(collection_info.buffers()[vmo_idx].vmo(), host_pointer_access_mode, callback,
                 vmo_bytes);
}

void MapHostPointer(const zx::vmo& vmo, HostPointerAccessMode host_pointer_access_mode,
                    std::function<void(uint8_t* mapped_ptr, uint32_t num_bytes)> callback,
                    uint64_t vmo_bytes) {
  if (vmo_bytes == 0) {
    auto status = vmo.get_prop_content_size(&vmo_bytes);
    // The content size is not always set, so when it's not available,
    // use the full VMO size.
    if (status != ZX_OK || vmo_bytes == 0) {
      vmo.get_size(&vmo_bytes);
    }
    FX_DCHECK(vmo_bytes > 0);
  }

  uint8_t* vmo_host = nullptr;
  const uint32_t vmo_options = HostPointerAccessModeToVmoOptions(host_pointer_access_mode);
  auto status = zx::vmar::root_self()->map(vmo_options, /*vmar_offset*/ 0, vmo, /*vmo_offset*/ 0,
                                           vmo_bytes, reinterpret_cast<uintptr_t*>(&vmo_host));
  FX_DCHECK(status == ZX_OK);

  if (host_pointer_access_mode == HostPointerAccessMode::kReadOnly ||
      host_pointer_access_mode == HostPointerAccessMode::kReadWrite) {
    // Flush the cache before reading back from the host VMO.
    status = zx_cache_flush(vmo_host, vmo_bytes, ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
    FX_DCHECK(status == ZX_OK);
  }

  callback(vmo_host, static_cast<uint32_t>(vmo_bytes));

  if (host_pointer_access_mode == HostPointerAccessMode::kWriteOnly ||
      host_pointer_access_mode == HostPointerAccessMode::kReadWrite) {
    // Flush the cache after writing to the host VMO.
    status = zx_cache_flush(vmo_host, vmo_bytes, ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
    FX_DCHECK(status == ZX_OK);
  }

  // Unmap the pointer.
  uintptr_t address = reinterpret_cast<uintptr_t>(vmo_host);
  status = zx::vmar::root_self()->unmap(address, vmo_bytes);
  FX_DCHECK(status == ZX_OK);
}

}  // namespace flatland
