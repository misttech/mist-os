// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/examples/screen_recording/screen_capture_helper.h"

#include "fuchsia/sysmem2/cpp/fidl.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "zircon/system/ulib/fbl/include/fbl/algorithm.h"

namespace screen_recording_example {

using flatland::MapHostPointer;
using fuchsia::ui::composition::RegisterBufferCollectionArgs;
using fuchsia::ui::composition::RegisterBufferCollectionUsages;

fuchsia::sysmem2::BufferCollectionInfo CreateBufferCollectionInfo2WithConstraints(
    fuchsia::sysmem2::BufferCollectionConstraints constraints,
    allocation::BufferCollectionExportToken export_token,
    fuchsia::ui::composition::Allocator_Sync* flatland_allocator,
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator, RegisterBufferCollectionUsages usage) {
  FX_DCHECK(flatland_allocator);
  FX_DCHECK(sysmem_allocator);

  RegisterBufferCollectionArgs rbc_args = {};

  zx_status_t status;
  // Create Sysmem tokens.
  auto [local_token, dup_token] = utils::CreateSysmemTokens(sysmem_allocator);

  rbc_args.set_export_token(std::move(export_token));
  // BufferCollectionToken zircon handles are interchangeable between fuchsia::sysmem2
  // and fuchsia::sysmem(1).
  rbc_args.set_buffer_collection_token(
      fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>(
          dup_token.Unbind().TakeChannel()));
  rbc_args.set_usages(usage);

  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_request;
  bind_request.set_buffer_collection_request(buffer_collection.NewRequest());
  bind_request.set_token(std::move(local_token));
  status = sysmem_allocator->BindSharedCollection(std::move(bind_request));
  FX_DCHECK(status == ZX_OK);

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest constraints_request;
  constraints_request.set_constraints(std::move(constraints));
  status = buffer_collection->SetConstraints(std::move(constraints_request));
  FX_DCHECK(status == ZX_OK);

  fuchsia::ui::composition::Allocator_RegisterBufferCollection_Result result;
  flatland_allocator->RegisterBufferCollection(std::move(rbc_args), &result);
  FX_DCHECK(!result.is_err());

  // Wait for allocation.
  zx_status_t allocation_status = ZX_OK;
  fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
  status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
  FX_DCHECK(ZX_OK == status);
  FX_DCHECK(ZX_OK == allocation_status);
  FX_DCHECK(wait_result.is_response());
  FX_DCHECK(constraints.min_buffer_count() ==
            wait_result.response().buffer_collection_info().buffers().size());

  buffer_collection->Release();
  return std::move(*wait_result.response().mutable_buffer_collection_info());
}

}  // namespace screen_recording_example
