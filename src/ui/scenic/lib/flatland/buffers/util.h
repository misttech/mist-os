// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_BUFFERS_UTIL_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_BUFFERS_UTIL_H_

#include "src/ui/scenic/lib/flatland/buffers/buffer_collection.h"

namespace flatland {

fuchsia::sysmem2::BufferUsage get_none_usage();

struct SysmemTokens {
  // Token for setting client side constraints.
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr local_token;

  // Token for setting server side constraints.
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr dup_token;

  static SysmemTokens Create(fuchsia::sysmem2::Allocator_Sync* sysmem_allocator) {
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr local_token;
    fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
    allocate_shared_request.set_token_request(local_token.NewRequest());
    zx_status_t status =
        sysmem_allocator->AllocateSharedCollection(std::move(allocate_shared_request));
    FX_DCHECK(status == ZX_OK);
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr dup_token;
    fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest dup_request;
    dup_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
    dup_request.set_token_request(dup_token.NewRequest());
    status = local_token->Duplicate(std::move(dup_request));
    FX_DCHECK(status == ZX_OK);
    fuchsia::sysmem2::Node_Sync_Result sync_result;
    status = local_token->Sync(&sync_result);
    FX_DCHECK(status == ZX_OK);
    FX_DCHECK(sync_result.is_response());
    return {std::move(local_token), std::move(dup_token)};
  }
};

// TODO(https://fxbug.dev/42132796): The default memory constraints set by Sysmem only allows using
// CPU domain for buffers with CPU usage, while Mali driver asks for only
// RAM and Inaccessible domains for buffer allocation, which caused failure in
// sysmem allocation. So here we add RAM domain support to clients in order
// to get buffer allocated correctly.
const std::pair<fuchsia::sysmem2::BufferUsage, fuchsia::sysmem2::BufferMemoryConstraints>
GetUsageAndMemoryConstraintsForCpuWriteOften();

// Sets the client constraints on a sysmem buffer collection, including the number of images,
// the dimensionality (width, height) of those images, the usage and memory constraints. This
// is a blocking function that will wait until the constraints have been fully set.
void SetClientConstraintsAndWaitForAllocated(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token, uint32_t image_count = 1,
    uint32_t width = 64, uint32_t height = 32,
    fuchsia::sysmem2::BufferUsage usage = fidl::Clone(get_none_usage()),
    const std::vector<fuchsia::images2::PixelFormatModifier>& additional_format_modifiers = {},
    std::optional<fuchsia::sysmem2::BufferMemoryConstraints> memory_constraints = std::nullopt);

// Sets the constraints on a client buffer collection pointer and returns that pointer back to
// the caller, *without* waiting for the constraint setting to finish. It is up to the caller
// to wait until constraints are set.
fuchsia::sysmem2::BufferCollectionSyncPtr CreateBufferCollectionSyncPtrAndSetConstraints(
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator,
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token, uint32_t image_count = 1,
    uint32_t width = 64, uint32_t height = 32,
    fuchsia::sysmem2::BufferUsage usage = fidl::Clone(get_none_usage()),
    fuchsia::images2::PixelFormat format = fuchsia::images2::PixelFormat::B8G8R8A8,
    std::optional<fuchsia::sysmem2::BufferMemoryConstraints> memory_constraints = std::nullopt,
    std::optional<fuchsia::images2::PixelFormatModifier> pixel_format_modifier = std::nullopt);

enum class HostPointerAccessMode : uint32_t {
  kReadOnly = 0b01,
  kWriteOnly = 0b10,
  kReadWrite = 0b11,
};

// Maps a sysmem vmo's bytes into host memory that can be accessed via a callback function. The
// callback provides the caller with a raw pointer to the vmo memory as well as an int for the
// number of bytes. If an out of bounds vmo_idx is provided, the callback function will call the
// user callback with mapped_ptr equal to nullptr. Once the callback function returns, the host
// pointer is unmapped and so cannot continue to be used outside of the scope of the callback.
void MapHostPointer(const fuchsia::sysmem2::BufferCollectionInfo& collection_info, uint32_t vmo_idx,
                    HostPointerAccessMode host_pointer_access_mode,
                    std::function<void(uint8_t* mapped_ptr, uint32_t num_bytes)> callback);

// Maps a given vmo's bytes into host memory that can be accessed via a callback function. The
// callback provides the caller with a raw pointer to the vmo memory as well as an int for the
// number of bytes. Once the callback function returns, the host
// pointer is unmapped and so cannot continue to be used outside of the scope of the callback.
void MapHostPointer(const zx::vmo& vmo, HostPointerAccessMode host_pointer_access_mode,
                    std::function<void(uint8_t* mapped_ptr, uint32_t num_bytes)> callback,
                    uint64_t vmo_bytes = 0);

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_BUFFERS_UTIL_H_
