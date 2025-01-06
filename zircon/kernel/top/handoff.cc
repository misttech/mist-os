// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/ticks.h>
#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/zbitl/view.h>
#include <platform.h>
#include <zircon/assert.h>
#include <zircon/rights.h>

#include <fbl/ref_ptr.h>
#include <ktl/byte.h>
#include <ktl/move.h>
#include <ktl/span.h>
#include <lk/init.h>
#include <object/handle.h>
#include <object/vm_object_dispatcher.h>
#include <phys/handoff.h>
#include <platform/boot_timestamps.h>
#include <platform/timer.h>
#include <vm/physmap.h>
#include <vm/vm_object_paged.h>

#include <ktl/enforce.h>

PhysHandoff* gPhysHandoff;

namespace {

// TODO(https://fxbug.dev/42164859): Eventually physboot will hand off a permanent pointer
// we can store in gBootOptions.  For now, handoff only provides temporary
// pointers that we must copy out of.
BootOptions gBootOptionsInstance;

// When using physboot, other samples are available in the handoff data too.
//
// **NOTE** Each sample here is represented in the userland test code in
// //src/tests/benchmarks/kernel_boot_stats.cc that knows the order of the
// steps and gives names to the intervals between the steps (as well as
// tracking the first-to-last total elapsed time across the first to last
// boot.timeline.* samples, not all recorded right here).  Any time a new time
// sample is added to PhysBootTimes, a kcounter should be added here and
// kernel_boot_stats.cc should be updated to give the new intervals appropriate
// names for the performance tracking infrastructure (see the pages at
// https://chromeperf.appspot.com/report and look for "fuchsia.kernel.boot").
KCOUNTER(timeline_hw_startup, "boot.timeline.hw")
KCOUNTER(timeline_zbi_entry, "boot.timeline.zbi")
KCOUNTER(timeline_physboot_setup, "boot.timeline.physboot-setup")
KCOUNTER(timeline_decompress_start, "boot.timeline.decompress-start")
KCOUNTER(timeline_decompress_end, "boot.timeline.decompress-end")
KCOUNTER(timeline_zbi_done, "boot.timeline.zbi-done")
KCOUNTER(timeline_physboot_handoff, "boot.timeline.physboot-handoff")
KCOUNTER(timeline_virtual_entry, "boot.timeline.virtual")

void Set(const Counter& counter, arch::EarlyTicks sample) {
  counter.Set(platform_convert_early_ticks(sample));
}

void Set(const Counter& counter, PhysBootTimes::Index i) {
  counter.Set(platform_convert_early_ticks(gPhysHandoff->times.Get(i)));
}

// Convert early boot timeline points into zx_ticks_t values in kcounters.
void TimelineCounters(unsigned int level) {
  // This isn't really a loop in any meaningful sense, but structuring it
  // this way gets the compiler to warn about any forgotten enum entry.
  for (size_t i = 0; i <= PhysBootTimes::kCount; ++i) {
    const PhysBootTimes::Index when = static_cast<PhysBootTimes::Index>(i);
    switch (when) {
      case PhysBootTimes::kZbiEntry:
        Set(timeline_zbi_entry, when);
        break;
      case PhysBootTimes::kPhysSetup:
        Set(timeline_physboot_setup, when);
        break;
      case PhysBootTimes::kDecompressStart:
        Set(timeline_decompress_start, when);
        break;
      case PhysBootTimes::kDecompressEnd:
        Set(timeline_decompress_end, when);
        break;
      case PhysBootTimes::kZbiDone:
        Set(timeline_zbi_done, when);
        break;
      case PhysBootTimes::kCount:
        // There is no PhysBootTimes entry corresponding to kCount.
        // This is the first sample taken by the kernel proper after physboot handed off.
        Set(timeline_physboot_handoff, kernel_entry_ticks);
        break;
    }
  }
  Set(timeline_virtual_entry, kernel_virtual_entry_ticks);
  Set(timeline_hw_startup, arch::EarlyTicks::Zero());
}

// This can happen really any time after the platform clock is configured.
LK_INIT_HOOK(TimelineCounters, TimelineCounters, LK_INIT_LEVEL_PLATFORM)

fbl::RefPtr<VmObject> CreatePhysVmo(const PhysVmo& phys_vmo) {
  ktl::string_view name{phys_vmo.name.data(), phys_vmo.name.size()};
  name = name.substr(0, name.find_first_of('\0'));
  DEBUG_ASSERT(!name.empty());

  DEBUG_ASSERT(IS_PAGE_ALIGNED(phys_vmo.addr));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(phys_vmo.size_bytes()));

  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::CreateFromWiredPages(paddr_to_physmap(phys_vmo.addr),
                                                           phys_vmo.size_bytes(), true, &vmo);
  ASSERT(status == ZX_OK);

  status = vmo->set_name(name.data(), name.size());
  DEBUG_ASSERT(status == ZX_OK);

  dprintf(INFO, "handing off VMO from phys: %.*s @ [%#" PRIx64 ", %#" PRIx64 ")\n",
          static_cast<int>(name.size()), name.data(), phys_vmo.addr,
          phys_vmo.addr + phys_vmo.content_size);

  return vmo;
}

HandleOwner CreateHandle(fbl::RefPtr<VmObject> vmo, size_t content_size, bool writable = false) {
  zx_rights_t rights;
  KernelHandle<VmObjectDispatcher> handle;
  zx_status_t status =
      VmObjectDispatcher::Create(ktl::move(vmo), content_size,
                                 VmObjectDispatcher::InitialMutability::kMutable, &handle, &rights);
  ASSERT(status == ZX_OK);

  if (writable) {
    rights |= ZX_RIGHT_WRITE;
  } else {
    rights &= ~ZX_RIGHT_WRITE;
  }
  return Handle::Make(ktl::move(handle), rights);
}

HandleOwner CreatePhysVmoHandle(const PhysVmo& phys_vmo, bool writable = false) {
  return CreateHandle(CreatePhysVmo(phys_vmo), phys_vmo.content_size, writable);
}

HandleOwner CreateStubVmoHandle() {
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, 0, &vmo);
  ASSERT(status == ZX_OK);
  return CreateHandle(ktl::move(vmo), 0);
}

}  // namespace

template <>
void* PhysHandoffPtrImportPhysAddr<PhysHandoffPtrEncoding::PhysAddr>(uintptr_t ptr) {
  return paddr_to_physmap(ptr);
}

void HandoffFromPhys(paddr_t handoff_paddr) {
  gPhysHandoff = static_cast<PhysHandoff*>(paddr_to_physmap(handoff_paddr));

  gBootOptionsInstance = *gPhysHandoff->boot_options;
  gBootOptions = &gBootOptionsInstance;

  if (gPhysHandoff->reboot_reason) {
    platform_set_hw_reboot_reason(gPhysHandoff->reboot_reason.value());
  }
}

HandoffEnd EndHandoff() {
  HandoffEnd end{
      // Userboot expects the ZBI as writable.
      .zbi = CreatePhysVmoHandle(gPhysHandoff->zbi, /*writable=*/true),
#ifndef __mist_os__
      .vdso = CreatePhysVmo(gPhysHandoff->vdso),
      .userboot = CreatePhysVmo(gPhysHandoff->userboot),
#endif
  };

  // If the number of extra VMOs from physboot is less than the number of VMOs
  // the userboot protocol expects, fill the rest with empty VMOs.
  ktl::span<const PhysVmo> phys_vmos = gPhysHandoff->extra_vmos.get();
  for (size_t i = 0; i < phys_vmos.size(); ++i) {
    end.extra_phys_vmos[i] = CreatePhysVmoHandle(phys_vmos[i]);
  }
  for (size_t i = phys_vmos.size(); i < PhysVmo::kMaxExtraHandoffPhysVmos; ++i) {
    end.extra_phys_vmos[i] = CreateStubVmoHandle();
  }

  // Point of temporary hand-off memory expiration. The PMM maintains the list
  // of such memory and will free it on this call. Since the memory backing
  // gPhysHandoff itself is in that list, we immediately unset immediately unset
  // the pointer afterward.
  pmm_end_handoff();
  gPhysHandoff = nullptr;

  return end;
}
