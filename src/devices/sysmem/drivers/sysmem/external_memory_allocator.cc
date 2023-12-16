// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "external_memory_allocator.h"

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>

#include <fbl/string_printf.h>

#include "macros.h"

namespace sysmem_driver {
ExternalMemoryAllocator::ExternalMemoryAllocator(
    MemoryAllocator::Owner* owner, fidl::WireSharedClient<fuchsia_hardware_sysmem::Heap> heap,
    fuchsia_hardware_sysmem::HeapProperties properties)
    : MemoryAllocator(properties), owner_(owner), heap_(std::move(heap)) {
  node_ = owner->heap_node()->CreateChild(
      fbl::StringPrintf("ExternalMemoryAllocator-%ld", id()).c_str());
  node_.CreateUint("id", id(), &properties_);
}

ExternalMemoryAllocator::~ExternalMemoryAllocator() { ZX_DEBUG_ASSERT(is_empty()); }

zx_status_t ExternalMemoryAllocator::Allocate(uint64_t size,
                                              const fuchsia_sysmem2::SingleBufferSettings& settings,
                                              std::optional<std::string> name,
                                              uint64_t buffer_collection_id, uint32_t buffer_index,
                                              zx::vmo* parent_vmo) {
  ZX_DEBUG_ASSERT_MSG(size % zx_system_get_page_size() == 0, "size: 0x%" PRIx64, size);
  ZX_DEBUG_ASSERT_MSG(
      fbl::round_up(*settings.buffer_settings()->size_bytes(), zx_system_get_page_size()) == size,
      "size_bytes: %" PRIu64 " size: 0x%" PRIx64, *settings.buffer_settings()->size_bytes(), size);
  // TODO(fxbug.dev/57690): We're currently using WireSharedClient for the combination of "shared"
  // and sync() being available, but once we remove OnRegister we should also evaluate whether we
  // can just use fidl::SyncClient.
  fidl::Arena arena;
  auto allocate_result = heap_.sync()->AllocateVmo(size, fidl::ToWire(arena, settings),
                                                   buffer_collection_id, buffer_index);
  if (!allocate_result.ok() || !allocate_result->is_ok()) {
    DRIVER_ERROR("Heap.AllocateVmo failed: %d %d", allocate_result.error().status(),
                 allocate_result.ok() ? allocate_result->error_value() : ZX_ERR_INTERNAL);
    // sanitize to ZX_ERR_NO_MEMORY regardless of why.
    return ZX_ERR_NO_MEMORY;
  }
  zx::vmo result_vmo = std::move(allocate_result->value()->vmo);
  fbl::String vmo_name;
  if (name.has_value()) {
    vmo_name = fbl::StringPrintf("sysmem-ext %s", name.value().c_str());
  } else {
    vmo_name = "sysmem-ext";
  }
  result_vmo.set_property(ZX_PROP_NAME, vmo_name.c_str(), vmo_name.length());
  allocations_.try_emplace(result_vmo.get(), BufferKey{.buffer_collection_id = buffer_collection_id,
                                                       .buffer_index = buffer_index});
  *parent_vmo = std::move(result_vmo);
  return ZX_OK;
}

void ExternalMemoryAllocator::Delete(zx::vmo parent_vmo) {
  auto it = allocations_.find(parent_vmo.get());
  if (it == allocations_.end()) {
    DRIVER_ERROR("Invalid allocation - vmo_handle: %d", parent_vmo.get());
    return;
  }
  BufferKey buffer_key = it->second;
  allocations_.erase(it);
  auto result = heap_.sync()->DeleteVmo(buffer_key.buffer_collection_id, buffer_key.buffer_index,
                                        std::move(parent_vmo));
  if (!result.ok()) {
    DRIVER_ERROR("HeapDestroyResource() failed - status: %d", result.status());
    // fall-through; the server also pays attention to ZX_VMO_ZERO_CHILDREN; if the server still
    // exists/existed it'll find out that way
  }
  if (is_empty()) {
    owner_->CheckForUnbind();
  }
}

}  // namespace sysmem_driver
