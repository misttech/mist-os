// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/screen_capture2/tests/common.h"

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/ui/scenic/lib/allocation/allocator.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_import_export_tokens.h"
#include "src/ui/scenic/lib/flatland/engine/engine.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture_buffer_collection_importer.h"
#include "src/ui/scenic/lib/utils/helpers.h"

using testing::_;

using allocation::Allocator;
using allocation::BufferCollectionImporter;
using fuchsia::ui::composition::RegisterBufferCollectionArgs;
using fuchsia::ui::composition::RegisterBufferCollectionUsages;
using screen_capture::ScreenCaptureBufferCollectionImporter;

namespace screen_capture2 {
namespace test {

std::shared_ptr<Allocator> CreateAllocator(
    std::shared_ptr<screen_capture::ScreenCaptureBufferCollectionImporter> importer,
    sys::ComponentContext* app_context) {
  std::vector<std::shared_ptr<BufferCollectionImporter>> extra_importers;
  std::vector<std::shared_ptr<BufferCollectionImporter>> screenshot_importers;
  screenshot_importers.push_back(importer);
  return std::make_shared<Allocator>(app_context, extra_importers, screenshot_importers,
                                     utils::CreateSysmemAllocatorSyncPtr("-allocator"));
}

void CreateBufferCollectionInfoWithConstraints(
    fuchsia::sysmem2::BufferCollectionConstraints constraints,
    allocation::BufferCollectionExportToken export_token,
    std::shared_ptr<Allocator> flatland_allocator,
    fuchsia::sysmem2::Allocator_Sync* sysmem_allocator) {
  RegisterBufferCollectionArgs rbc_args = {};

  zx_status_t status;
  // Create Sysmem tokens.
  auto [local_token, dup_token] = utils::CreateSysmemTokens(sysmem_allocator);

  rbc_args.set_export_token(std::move(export_token));
  rbc_args.set_buffer_collection_token(
      fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>(
          dup_token.Unbind().TakeChannel()));
  rbc_args.set_usages(RegisterBufferCollectionUsages::SCREENSHOT);

  fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(local_token));
  bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
  status = sysmem_allocator->BindSharedCollection(std::move(bind_shared_request));
  FX_DCHECK(status == ZX_OK);

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.set_constraints(std::move(constraints));
  status = buffer_collection->SetConstraints(std::move(set_constraints_request));
  EXPECT_EQ(status, ZX_OK);

  bool processed_callback = false;
  flatland_allocator->RegisterBufferCollection(
      std::move(rbc_args),
      [&processed_callback](
          fuchsia::ui::composition::Allocator_RegisterBufferCollection_Result result) {
        EXPECT_EQ(false, result.is_err());
        processed_callback = true;
      });

  // Wait for allocation.
  fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
  status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
  ASSERT_EQ(ZX_OK, status);
  ASSERT_TRUE(!wait_result.is_framework_err());
  ASSERT_TRUE(!wait_result.is_err());
  ASSERT_TRUE(wait_result.is_response());
  auto buffer_collection_info = std::move(*wait_result.response().mutable_buffer_collection_info());
  ASSERT_EQ(constraints.min_buffer_count(), buffer_collection_info.buffers().size());

  buffer_collection->Release();
}

}  // namespace test
}  // namespace screen_capture2
