// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "kernel/restricted_state.h"

#include <lib/fit/defer.h>
#include <trace.h>

#include <vm/vm_address_region.h>
#include <vm/vm_object_paged.h>

#define LOCAL_TRACE 0

static constexpr size_t kStateVmoSize = PAGE_SIZE;

zx::result<ktl::unique_ptr<RestrictedState>> RestrictedState::Create(
    fbl::RefPtr<AttributionObject> attribution_object) {
  // Create a VMO.
  static constexpr uint32_t kVmoOptions = 0;
  static constexpr uint32_t kPmmAllocFlags = PMM_ALLOC_FLAG_ANY | PMM_ALLOC_FLAG_CAN_WAIT;
  fbl::RefPtr<VmObjectPaged> state_vmo;
  zx_status_t status = VmObjectPaged::Create(kPmmAllocFlags, kVmoOptions, kStateVmoSize,
                                             ktl::move(attribution_object), &state_vmo);
  if (status != ZX_OK) {
    return zx::error_result(status);
  }

  DEBUG_ASSERT(state_vmo->is_paged());
  DEBUG_ASSERT(!state_vmo->is_resizable());
  DEBUG_ASSERT(!state_vmo->is_discardable());
  DEBUG_ASSERT(!state_vmo->is_user_pager_backed());
  DEBUG_ASSERT(state_vmo->GetMappingCachePolicy() == ZX_CACHE_POLICY_CACHED);

  // Commit and pin the VMO.
  status = state_vmo->CommitRangePinned(0, kStateVmoSize, /*write=*/true);
  if (status != ZX_OK) {
    return zx::error_result(status);
  }
  auto unpin = fit::defer([&]() { state_vmo->Unpin(0, kStateVmoSize); });

  // Create a mapping of this VMO in the kernel aspace.
  fbl::RefPtr<VmAddressRegion> kernel_vmar =
      VmAspace::kernel_aspace()->RootVmar()->as_vm_address_region();
  fbl::RefPtr<VmMapping> state_mapping;
  status = kernel_vmar->CreateVmMapping(0, kStateVmoSize, 0, 0, state_vmo, 0,
                                        ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE,
                                        "restricted state", &state_mapping);
  if (status != ZX_OK) {
    return zx::error_result(status);
  }
  auto unmap = fit::defer([&]() { state_mapping->Destroy(); });

  LTRACEF("%s mapping at %#" PRIxPTR "\n", "", state_mapping->base_locking());

  // Eagerly fault in all the pages so we don't demand fault the mapping.
  status = state_mapping->MapRange(0, kStateVmoSize, true);
  if (status != ZX_OK) {
    return zx::error_result(status);
  }

  fbl::AllocChecker ac;
  ktl::unique_ptr<RestrictedState> rs(
      new (&ac) RestrictedState(std::move(state_vmo), std::move(state_mapping)));
  if (!ac.check()) {
    return zx::error_result(ZX_ERR_NO_MEMORY);
  }

  unmap.cancel();
  unpin.cancel();
  return zx::ok(ktl::move(rs));
}

RestrictedState::RestrictedState(fbl::RefPtr<VmObjectPaged> state_vmo,
                                 fbl::RefPtr<VmMapping> state_mapping)
    : state_vmo_(ktl::move(state_vmo)),
      state_mapping_(ktl::move(state_mapping)),
      state_mapping_ptr_(reinterpret_cast<void*>(state_mapping_->base_locking())) {
  DEBUG_ASSERT(is_kernel_address(reinterpret_cast<vaddr_t>(state_mapping_ptr_)));
}

RestrictedState::~RestrictedState() {
  zx_status_t status = state_mapping_->Destroy();
  DEBUG_ASSERT(status == ZX_OK);
  state_vmo_->Unpin(0, state_vmo_->size());
}

fbl::RefPtr<VmObjectPaged> RestrictedState::vmo() const { return state_vmo_; }
