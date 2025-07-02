// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_HANDOFF_END_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_HANDOFF_END_H_

// These functions relate to PhysHandoff but exist only in the kernel proper.

#include <stddef.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/type_traits.h>
#include <object/handle.h>
#include <phys/handoff.h>

// Forward declaration; defined in <vm/vm_object.h>
class VmObject;

// Called as soon as the kernel is entered to set the gPhysHandoff pointer.
void HandoffFromPhys(PhysHandoff* handoff);

// Valid to call only after HandoffFromPhys().
paddr_t KernelPhysicalLoadAddress();

// This is valid to call only with addresses inside the kernel image itself.
// The template wrappers below should always be used instead.  They ensure at
// compile time that only a variable or function in the kernel's image is used.
paddr_t KernelPhysicalAddressOf(uintptr_t va);

// The template argument can be any global / static function or variable.
template <auto& Symbol>
inline paddr_t KernelPhysicalAddressOf() {
  return KernelPhysicalAddressOf(reinterpret_cast<uintptr_t>(&Symbol));
}

// This version can be used to index into an array variable.
template <auto& Symbol>
  requires(ktl::is_array_v<ktl::remove_reference_t<decltype(Symbol)>>)
inline paddr_t KernelPhysicalAddressOf(size_t idx) {
  return KernelPhysicalAddressOf(reinterpret_cast<uintptr_t>(&Symbol[idx]));
}

// The remaining hand-off data to be consumed at the end of the hand-off phase
// (see EndHandoff()).
struct HandoffEnd {
  // Culled from PhysElfImage.
  struct Elf {
    fbl::RefPtr<VmObject> vmo;
    size_t content_size;
    size_t vmar_size;
    fbl::Vector<PhysMapping> mappings;
    PhysElfImage::Info info;
  };

  // The data ZBI.
  HandleOwner zbi;

  Elf vdso;
  Elf userboot;

  // The VMOs deriving from the phys environment. As returned by EndHandoff(),
  // the entirety of the array will be populated by real handles (if only by
  // stub VMOs) (as is convenient for userboot, its intended caller).
  std::array<HandleOwner, PhysVmo::kMaxExtraHandoffPhysVmos> extra_phys_vmos;
};

// Formally ends the hand-off phase, unsetting gPhysHandoff and returning the
// remaining hand-off data left to be consumed (in a userboot-friendly way),
// and freeing temporary hand-off memory (see PhysHandoff::temporary_memory).
//
// After the end of hand-off, all pointers previously referenced by
// gPhysHandoff should be regarded as freed and unusable.
HandoffEnd EndHandoff();

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_HANDOFF_END_H_
