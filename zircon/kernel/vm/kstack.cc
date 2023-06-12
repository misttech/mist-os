// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "vm/kstack.h"

#include <assert.h>
#include <inttypes.h>
#include <lib/counters.h>
#include <lib/fit/defer.h>
#include <stdio.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <ktl/move.h>
#include <vm/vm.h>
#include <vm/vm_address_region.h>
#include <vm/vm_aspace.h>
#include <vm/vm_object_paged.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace {

struct StackType {
  const char* name;
  size_t size;
};

KCOUNTER(vm_kernel_stack_bytes, "vm.kstack.allocated_bytes")

constexpr StackType kSafe = {"kernel-safe-stack", DEFAULT_STACK_SIZE};
#if __has_feature(safe_stack)
constexpr StackType kUnsafe = {"kernel-unsafe-stack", DEFAULT_STACK_SIZE};
#endif
#if __has_feature(shadow_call_stack)
constexpr StackType kShadowCall = {"kernel-shadow-call-stack", ZX_PAGE_SIZE};
#endif

constexpr size_t kStackPaddingSize = PAGE_SIZE;

// Takes a portion of the VMO and maps a kernel stack with one page of padding before and after the
// mapping.
zx_status_t map(const StackType& type, fbl::RefPtr<VmObjectPaged>& vmo, uint64_t* offset,
                KernelStack::Mapping* map) {
  LTRACEF("allocating %s\n", type.name);

  // assert that this mapping hasn't already be created
  DEBUG_ASSERT(!map->vmar_);

  // get a handle to the root vmar
  auto vmar = VmAspace::kernel_aspace()->RootVmar()->as_vm_address_region();
  DEBUG_ASSERT(!!vmar);

  // create a vmar with enough padding for a page before and after the stack
  fbl::RefPtr<VmAddressRegion> kstack_vmar;
  zx_status_t status = vmar->CreateSubVmar(
      0, 2 * kStackPaddingSize + type.size, 0,
      VMAR_FLAG_CAN_MAP_SPECIFIC | VMAR_FLAG_CAN_MAP_READ | VMAR_FLAG_CAN_MAP_WRITE, type.name,
      &kstack_vmar);
  if (status != ZX_OK) {
    return status;
  }

  // destroy the vmar if we early abort
  // this will also clean up any mappings that may get placed on the vmar
  auto vmar_cleanup = fit::defer([&kstack_vmar]() { kstack_vmar->Destroy(); });

  LTRACEF("%s vmar at %#" PRIxPTR "\n", type.name, kstack_vmar->base());

  // create a mapping offset kStackPaddingSize into the vmar we created
  fbl::RefPtr<VmMapping> kstack_mapping;
  status = kstack_vmar->CreateVmMapping(kStackPaddingSize, type.size, 0, VMAR_FLAG_SPECIFIC, vmo,
                                        *offset, ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE,
                                        type.name, &kstack_mapping);
  if (status != ZX_OK) {
    return status;
  }

  LTRACEF("%s mapping at %#" PRIxPTR "\n", type.name, kstack_mapping->base_locked());

  // fault in all the pages so we dont demand fault in the stack
  status = kstack_mapping->MapRange(0, type.size, true);
  if (status != ZX_OK) {
    return status;
  }
  vm_kernel_stack_bytes.Add(type.size);

  // Cancel the cleanup handler on the vmar since we're about to save a
  // reference to it.
  vmar_cleanup.cancel();

  // save the relevant bits
  map->vmar_ = ktl::move(kstack_vmar);

  // Increase the offset to claim this portion of the VMO.
  *offset += type.size;

  return ZX_OK;
}

}  // namespace

vaddr_t KernelStack::Mapping::base() const {
  // The actual stack mapping starts after the padding.
  return vmar_ ? vmar_->base() + kStackPaddingSize : 0;
}

size_t KernelStack::Mapping::size() const {
  // Remove the padding from the vmar to get the actual stack size.
  return vmar_ ? vmar_->size() - kStackPaddingSize * 2 : 0;
}

zx_status_t KernelStack::Init() {
  // Determine the total VMO size we needed for all stacks.
  size_t vmo_size = kSafe.size;
#if __has_feature(safe_stack)
  vmo_size += kUnsafe.size;
#endif

#if __has_feature(shadow_call_stack)
  vmo_size += kShadowCall.size;
#endif

  // Create a VMO for our stacks. Although multiple stacks will be allocated from adjacent blocks of
  // the VMO, they are only referenced by their virtual mapping addresses, and so there is no
  // possibility of over or under run of any stack trampling an adjacent one, they will just fault
  // on the guard regions around the mappings. Similarly the mapping location of each stack is
  // randomized independently, so allocating out of the same VMO provides no correlation to the
  // mapped addresses.
  // Using a single VMO reduces bookkeeping memory overhead with no downside, since all stacks have
  // exactly the same lifetime.
  // TODO(fxbug.dev/128913): VMOs containing kernel stacks for user threads should be linked to the
  // attribution objects of the corresponding processes.
  fbl::RefPtr<VmObjectPaged> stack_vmo;
  zx_status_t status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kAlwaysPinned, vmo_size,
                            AttributionObject::GetKernelAttribution(), &stack_vmo);
  if (status != ZX_OK) {
    LTRACEF("error allocating kernel stacks for thread\n");
    return status;
  }
  constexpr const char kKernelStackName[] = "kernel-stack";
  stack_vmo->set_name(kKernelStackName, sizeof(kKernelStackName) - 1);

  uint64_t vmo_offset = 0;
  status = map(kSafe, stack_vmo, &vmo_offset, &main_map_);
  if (status != ZX_OK) {
    return status;
  }

#if __has_feature(safe_stack)
  DEBUG_ASSERT(!unsafe_map_.vmar_);
  status = map(kUnsafe, stack_vmo, &vmo_offset, &unsafe_map_);
  if (status != ZX_OK) {
    return status;
  }
#endif

#if __has_feature(shadow_call_stack)
  DEBUG_ASSERT(!shadow_call_map_.vmar_);
  status = map(kShadowCall, stack_vmo, &vmo_offset, &shadow_call_map_);
  if (status != ZX_OK) {
    return status;
  }
#endif
  return ZX_OK;
}

void KernelStack::DumpInfo(int debug_level) const {
  auto map_dump = [debug_level](const KernelStack::Mapping& map, const char* tag) {
    dprintf(debug_level, "\t%s base %#" PRIxPTR ", size %#zx, vmar %p\n", tag, map.base(),
            map.size(), map.vmar_.get());
  };

  map_dump(main_map_, "stack");
#if __has_feature(safe_stack)
  map_dump(unsafe_map_, "unsafe_stack");
#endif
#if __has_feature(shadow_call_stack)
  map_dump(shadow_call_map_, "shadow_call_stack");
#endif
}

KernelStack::~KernelStack() {
  [[maybe_unused]] zx_status_t status = Teardown();
  DEBUG_ASSERT_MSG(status == ZX_OK, "KernelStack::Teardown returned %d\n", status);
}

zx_status_t KernelStack::Teardown() {
  if (main_map_.vmar_) {
    LTRACEF("removing vmar at at %#" PRIxPTR "\n", main_map_.vmar_->base());
    zx_status_t status = main_map_.vmar_->Destroy();
    if (status != ZX_OK) {
      return status;
    }
    main_map_.vmar_.reset();
    vm_kernel_stack_bytes.Add(-static_cast<int64_t>(kSafe.size));
  }
#if __has_feature(safe_stack)
  if (unsafe_map_.vmar_) {
    LTRACEF("removing unsafe vmar at at %#" PRIxPTR "\n", unsafe_map_.vmar_->base());
    zx_status_t status = unsafe_map_.vmar_->Destroy();
    if (status != ZX_OK) {
      return status;
    }
    unsafe_map_.vmar_.reset();
    vm_kernel_stack_bytes.Add(-static_cast<int64_t>(kUnsafe.size));
  }
#endif
#if __has_feature(shadow_call_stack)
  if (shadow_call_map_.vmar_) {
    LTRACEF("removing shadow call vmar at at %#" PRIxPTR "\n", shadow_call_map_.vmar_->base());
    zx_status_t status = shadow_call_map_.vmar_->Destroy();
    if (status != ZX_OK) {
      return status;
    }
    shadow_call_map_.vmar_.reset();
    vm_kernel_stack_bytes.Add(-static_cast<int64_t>(kShadowCall.size));
  }
#endif
  return ZX_OK;
}
