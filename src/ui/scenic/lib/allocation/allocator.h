// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_ALLOCATION_ALLOCATOR_H_
#define SRC_UI_SCENIC_LIB_ALLOCATION_ALLOCATOR_H_

#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fuchsia/sysmem2/cpp/fidl.h>
#include <lib/sys/cpp/component_context.h>

#include <memory>
#include <unordered_map>

#include "src/lib/fxl/memory/weak_ptr.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/allocation/id.h"

namespace allocation {

// This class implements Allocator service which allows allocation of BufferCollections which can be
// used in multiple Flatland/Gfx sessions simultaneously.
class Allocator : public fidl::Server<fuchsia_ui_composition::Allocator> {
 public:
  Allocator(sys::ComponentContext* app_context,
            const std::vector<std::shared_ptr<BufferCollectionImporter>>&
                default_buffer_collection_importers,
            const std::vector<std::shared_ptr<BufferCollectionImporter>>&
                screenshot_buffer_collection_importers,
            fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator);
  ~Allocator() override;

  // |fuchsia_ui_composition::Allocator|
  void RegisterBufferCollection(RegisterBufferCollectionRequest& request,
                                RegisterBufferCollectionCompleter::Sync& completer) override;
  void RegisterBufferCollection(
      fuchsia_ui_composition::RegisterBufferCollectionArgs args,
      fit::function<void(fit::result<fuchsia_ui_composition::RegisterBufferCollectionError>)>
          completer);

 private:
  void ReleaseBufferCollection(GlobalBufferCollectionId collection_id);

  // Returns a list of references to all importers to be used for a buffer collection with |usages|.
  std::vector<std::pair<BufferCollectionImporter&, allocation::BufferCollectionUsage>> GetImporters(
      fuchsia_ui_composition::RegisterBufferCollectionUsages usages) const;

  // Dispatcher where this class runs on. Currently points to scenic main thread's dispatcher.
  async_dispatcher_t* dispatcher_;

  // The FIDL bindings for this Allocator instance, which reference |this| as the implementation and
  // run on |dispatcher_|.
  fidl::ServerBindingGroup<fuchsia_ui_composition::Allocator> bindings_;

  // Used to import Flatland buffer collections and images to external services that Flatland does
  // not have knowledge of. Each importer is used for a different service.
  std::vector<std::shared_ptr<BufferCollectionImporter>> default_buffer_collection_importers_;

  // Used to import buffer collections for screenshot purposes.
  std::vector<std::shared_ptr<BufferCollectionImporter>> screenshot_buffer_collection_importers_;

  // A Sysmem allocator to facilitate buffer allocation with the Renderer.
  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;

  // Keep track of buffer collection Ids for garbage collection.
  std::unordered_map<GlobalBufferCollectionId,
                     fuchsia_ui_composition::RegisterBufferCollectionUsages>
      buffer_collections_;

  // Should be last.
  fxl::WeakPtrFactory<Allocator> weak_factory_;
};

}  // namespace allocation

#endif  // SRC_UI_SCENIC_LIB_ALLOCATION_ALLOCATOR_H_
