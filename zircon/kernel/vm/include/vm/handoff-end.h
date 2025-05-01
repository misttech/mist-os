// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_HANDOFF_END_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_HANDOFF_END_H_

// These functions relate to PhysHandoff but exist only in the kernel proper.

#include <stddef.h>

#include <fbl/ref_ptr.h>
#include <object/handle.h>
#include <phys/handoff.h>

// Forward declaration; defined in <vm/vm_object.h>
class VmObject;

// Called as soon as the physmap is available to set the gPhysHandoff pointer.
void HandoffFromPhys(paddr_t handoff_paddr);

// Valid to call only after HandoffFromPhys().
paddr_t KernelPhysicalLoadAddress();

// The remaining hand-off data to be consumed at the end of the hand-off phase
// (see EndHandoff()).
struct HandoffEnd {
  // The data ZBI.
  HandleOwner zbi;

  fbl::RefPtr<VmObject> vdso;
  fbl::RefPtr<VmObject> userboot;

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
