// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "vm/vm_object_physical.h"

#include <align.h>
#include <assert.h>
#include <inttypes.h>
#include <lib/console.h>
#include <stdlib.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <ktl/move.h>
#include <vm/physmap.h>
#include <vm/vm.h>
#include <vm/vm_address_region.h>

#include "vm_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE VM_GLOBAL_TRACE(0)

VmObjectPhysical::VmObjectPhysical(fbl::RefPtr<VmHierarchyState> state, paddr_t base, uint64_t size,
                                   bool is_slice, uint64_t parent_user_id)
    : VmObject(VMOType::Physical, ktl::move(state)),
      size_(size),
      base_(base),
      is_slice_(is_slice),
      parent_user_id_(parent_user_id) {
  LTRACEF("%p, size %#" PRIx64 "\n", this, size_);

  DEBUG_ASSERT(IS_PAGE_ALIGNED(size_));

  AddToGlobalList();
}

VmObjectPhysical::~VmObjectPhysical() {
  canary_.Assert();
  LTRACEF("%p\n", this);

  {
    Guard<CriticalMutex> guard{ChildListLock::Get()};
    if (parent_) {
      parent_->RemoveChild(this, guard.take());
      // Avoid recursing destructors when we delete our parent by using the deferred deletion
      // method.
      VmDeferredDeleter<VmObjectPhysical>::DoDeferredDelete(ktl::move(parent_));
    }
  }

  RemoveFromGlobalList();
}

zx_status_t VmObjectPhysical::Create(paddr_t base, uint64_t size,
                                     fbl::RefPtr<VmObjectPhysical>* obj) {
  if (!IS_PAGE_ALIGNED(base) || !IS_PAGE_ALIGNED(size) || size == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  // check that base + size is a valid range
  paddr_t safe_base;
  if (add_overflow(base, size - 1, &safe_base)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  auto state = fbl::AdoptRef<VmHierarchyState>(new (&ac) VmHierarchyState);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto vmo = fbl::AdoptRef<VmObjectPhysical>(new (&ac) VmObjectPhysical(
      ktl::move(state), base, size, /*is_slice=*/false, /*parent_user_id=*/0));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // Physical VMOs should default to uncached access.
  vmo->SetMappingCachePolicy(ARCH_MMU_FLAG_UNCACHED);

  *obj = ktl::move(vmo);

  return ZX_OK;
}

zx_status_t VmObjectPhysical::CreateChildSlice(uint64_t offset, uint64_t size, bool copy_name,
                                               fbl::RefPtr<VmObject>* child_vmo) {
  canary_.Assert();

  // Offset must be page aligned.
  if (!IS_PAGE_ALIGNED(offset)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Make sure size is page aligned.
  if (!IS_PAGE_ALIGNED(size)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (size > MAX_SIZE) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // Forbid creating children of resizable VMOs. This restriction may be lifted in the future.
  if (is_resizable()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Slice must be wholly contained.
  uint64_t our_size;
  {
    // size_ is not an atomic variable and although it should not be changing, as we are not
    // allowing this operation on resizable vmo's, we should still be holding the lock to
    // correctly read size_. Unfortunately we must also drop then drop the lock in order to
    // perform the allocation.
    Guard<VmoLockType> guard{lock()};
    our_size = size_;
  }
  if (!InRange(offset, size, our_size)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // To mimic a slice we can just create a physical vmo with the correct region. This works since
  // nothing is resizable and the slice must be wholly contained.
  // We can read and store the user_id here since for a slice to be being created the dispatcher
  // side of this object must have completed, and hence the user_id has been set.
  fbl::AllocChecker ac;
  auto vmo = fbl::AdoptRef<VmObjectPhysical>(new (&ac) VmObjectPhysical(
      hierarchy_state_ptr_, base_ + offset, size, /*is_slice=*/true, user_id()));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  {
    Guard<VmoLockType> guard{lock()};

    // Inherit the current cache policy
    vmo->mapping_cache_flags_ = mapping_cache_flags_;
    // Initialize parent
    vmo->parent_ = fbl::RefPtr(this);

    // add the new vmo as a child.
    AddChild(vmo.get());

    if (copy_name) {
      vmo->name_ = name_;
    }
  }

  *child_vmo = ktl::move(vmo);

  return ZX_OK;
}

void VmObjectPhysical::Dump(uint depth, bool verbose) {
  canary_.Assert();

  Guard<VmoLockType> guard{lock()};
  for (uint i = 0; i < depth; ++i) {
    printf("  ");
  }
  printf("object %p base %#" PRIxPTR " size %#" PRIx64 " ref %d\n", this, base_, size_,
         ref_count_debug());
}

zx_status_t VmObjectPhysical::Lookup(uint64_t offset, uint64_t len,
                                     VmObject::LookupFunction lookup_fn) {
  canary_.Assert();

  if (unlikely(len == 0)) {
    return ZX_ERR_INVALID_ARGS;
  }

  Guard<VmoLockType> guard{lock()};
  if (unlikely(!InRange(offset, len, size_))) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  uint64_t cur_offset = ROUNDDOWN(offset, PAGE_SIZE);
  uint64_t end = offset + len;
  uint64_t end_page_offset = ROUNDUP(end, PAGE_SIZE);

  for (size_t idx = 0; cur_offset < end_page_offset; cur_offset += PAGE_SIZE, ++idx) {
    zx_status_t status = lookup_fn(cur_offset, base_ + cur_offset);
    if (unlikely(status != ZX_ERR_NEXT)) {
      if (status == ZX_ERR_STOP) {
        return ZX_OK;
      }
      return status;
    }
  }
  return ZX_OK;
}

zx_status_t VmObjectPhysical::CommitRangePinned(uint64_t offset, uint64_t len, bool write) {
  canary_.Assert();

  if (unlikely(len == 0 || !IS_PAGE_ALIGNED(offset))) {
    return ZX_ERR_INVALID_ARGS;
  }
  Guard<VmoLockType> guard{lock()};
  if (unlikely(!InRange(offset, len, size_))) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  // Physical VMOs are always committed and so are always pinned.
  return ZX_OK;
}

zx_status_t VmObjectPhysical::PrefetchRange(uint64_t offset, uint64_t len) {
  canary_.Assert();

  if (!InRange(offset, len, size_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (len == 0) {
    return ZX_OK;
  }
  return ZX_OK;
}

zx_status_t VmObjectPhysical::LookupContiguous(uint64_t offset, uint64_t len, paddr_t* out_paddr) {
  Guard<VmoLockType> guard{lock()};
  return LookupContiguousLocked(offset, len, out_paddr);
}

zx_status_t VmObjectPhysical::LookupContiguousLocked(uint64_t offset, uint64_t len,
                                                     paddr_t* out_paddr) {
  canary_.Assert();
  if (unlikely(len == 0 || !IS_PAGE_ALIGNED(offset))) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (unlikely(!InRange(offset, len, size_))) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (out_paddr) {
    *out_paddr = base_ + offset;
  }
  return ZX_OK;
}

uint32_t VmObjectPhysical::GetMappingCachePolicyLocked() const { return mapping_cache_flags_; }

zx_status_t VmObjectPhysical::SetMappingCachePolicy(const uint32_t cache_policy) {
  // Is it a valid cache flag?
  if (cache_policy & ~ZX_CACHE_POLICY_MASK) {
    return ZX_ERR_INVALID_ARGS;
  }

  Guard<VmoLockType> guard{lock()};

  // If the cache policy is already configured on this VMO and matches
  // the requested policy then this is a no-op. This is a common practice
  // in the serialio and magma drivers, but may change.
  // TODO: revisit this when we shake out more of the future DDK protocol.
  if (cache_policy == mapping_cache_flags_) {
    return ZX_OK;
  }

  Guard<CriticalMutex> list_guard{ChildListLock::Get()};
  // If this VMO is mapped already it is not safe to allow its caching policy to change
  if (self_locked()->num_mappings_locked() != 0 || children_list_len_ != 0 || parent_) {
    LTRACEF(
        "Warning: trying to change cache policy while this vmo has mappings, children or a "
        "parent!\n");
    return ZX_ERR_BAD_STATE;
  }

  mapping_cache_flags_ = cache_policy;
  return ZX_OK;
}
