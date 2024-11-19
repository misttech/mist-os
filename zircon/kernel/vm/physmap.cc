// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "vm/physmap.h"

#include <lib/fit/function.h>
#include <trace.h>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>
#include <phys/handoff.h>
#include <vm/pmm.h>

#include "vm_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE VM_GLOBAL_TRACE(0)

namespace {

// Permissions & flags for regions of the physmap backed by memory. Execute permissions
// are not included - we do not ever execute from physmap addresses.
constexpr uint kPhysmapMmuFlags = ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE;

// Permissions & flags for regions of the physmap that are not backed by memory; they
// may represent MMIOs or non-allocatable (ACPI NVS) memory. The kernel may access
// some peripherals in these addresses (such as MMIO-based UARTs) in early boot.
// ARM has its own periphmap area for peripherals and can tolerate a full unmap.
// x86 and riscv may need to access MMIO based UARTs, and if so will call
// |physmap_preserve_gaps_for_mmio| hence by default assume can fully unmap.
uint gap_mmu_flags = 0;

// Changes the flags for the region [ |base|, |base| + |size| ) in the physmap.
// If |mmu_flags| is 0, the region is unmapped.
void physmap_modify_region(vaddr_t base, size_t size, uint mmu_flags) {
  DEBUG_ASSERT(base % PAGE_SIZE == 0);
  DEBUG_ASSERT(size % PAGE_SIZE == 0);
  const size_t page_count = size / PAGE_SIZE;
  LTRACEF("base=0x%" PRIx64 "; page_count=0x%" PRIx64 "\n", base, page_count);

  zx_status_t status;
  if (mmu_flags != 0) {
    // This code only runs during the init stages before other CPUs are brought only, and so we are
    // safe to allow temporary enlargement of the operation.
    status = VmAspace::kernel_aspace()->arch_aspace().Protect(base, page_count, mmu_flags,
                                                              ArchVmAspace::EnlargeOperation::Yes);
  } else {
    // This code only runs during the init stages before other CPUs are brought only, and so we are
    // safe to allow temporary enlargement of the operation.
    size_t unmapped;
    status = VmAspace::kernel_aspace()->arch_aspace().Unmap(
        base, page_count, ArchVmAspace::EnlargeOperation::Yes, &unmapped);
  }
  ASSERT(status == ZX_OK);
}

void physmap_protect_gap(vaddr_t base, size_t size) {
  // Ideally, we'd drop the range completely, but early boot code currently relies
  // on peripherals being mapped in.
  //
  // TODO(https://fxbug.dev/42124648): Remove these regions completely.
  if (gap_mmu_flags & (ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE)) {
    printf("WARNING physmap not fully unmapping non arena area [%#lx, %#lx)\n", base, base + size);
  }
  physmap_modify_region(base, size, gap_mmu_flags);
}

}  // namespace

void physmap_preserve_gaps_for_mmio() {
  gap_mmu_flags =
      ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE | ARCH_MMU_FLAG_UNCACHED_DEVICE;
}

void physmap_for_each_gap(fit::inline_function<void(vaddr_t base, size_t size)> func,
                          pmm_arena_info_t* arenas, size_t num_arenas,
                          ktl::optional<zbi_nvram_t> nvram) {
  // Call the provided func on the provided gap, making sure to avoid any NVRAM
  // range. (See function docstring.)
  auto call_on_gap = [func = ktl::move(func), nvram](vaddr_t base, size_t size) {
    if (size == 0) {
      return;
    }
    if (!nvram) {
      func(base, size);
      return;
    }

    vaddr_t end = base + size;
    vaddr_t nvram_base = reinterpret_cast<vaddr_t>(paddr_to_physmap(nvram->base));
    vaddr_t nvram_end = nvram_base + nvram->length;
    if (nvram_end <= base || end <= nvram_base) {  // No overlap; gap is unbroken.
      func(base, size);
      return;
    }
    if (base < nvram_base) {  // Gap leading up to the NVRAM range.
      func(base, nvram_base - base);
    }
    if (nvram_end < end) {  // Gap after the NVRAM range.
      func(nvram_end, end - nvram_end);
    }
  };

  // Iterate over the arenas and invoke |func| for the gaps between them.
  //
  // |gap_base| is the base address of the last identified gap.
  vaddr_t gap_base = PHYSMAP_BASE;
  for (unsigned i = 0; i < num_arenas; ++i) {
    const vaddr_t arena_base = reinterpret_cast<vaddr_t>(paddr_to_physmap(arenas[i].base));
    DEBUG_ASSERT(arena_base >= gap_base && arena_base % PAGE_SIZE == 0);

    const size_t arena_size = arenas[i].size;
    DEBUG_ASSERT(arena_size > 0 && arena_size % PAGE_SIZE == 0);

    LTRACEF("gap_base=%" PRIx64 "; arena_base=%" PRIx64 "; arena_size=%" PRIx64 "\n", gap_base,
            arena_base, arena_size);

    const size_t gap_size = arena_base - gap_base;
    call_on_gap(gap_base, gap_size);
    gap_base = arena_base + arena_size;
  }

  // Don't forget the last gap.
  const vaddr_t physmap_end = PHYSMAP_BASE + PHYSMAP_SIZE;
  const size_t gap_size = physmap_end - gap_base;
  if (gap_size > 0) {
    call_on_gap(gap_base, gap_size);
  }
}

void physmap_protect_non_arena_regions() {
  // Create a buffer to hold the pmm_arena_info_t objects.
  const size_t num_arenas = pmm_num_arenas();
  fbl::AllocChecker ac;
  auto arenas = ktl::unique_ptr<pmm_arena_info_t[]>(new (&ac) pmm_arena_info_t[num_arenas]);
  ASSERT(ac.check());
  const size_t size = num_arenas * sizeof(pmm_arena_info_t);

  // Fetch them.
  zx_status_t status = pmm_get_arena_info(num_arenas, 0, arenas.get(), size);
  ASSERT(status == ZX_OK);

  physmap_for_each_gap(physmap_protect_gap, arenas.get(), num_arenas, gPhysHandoff->nvram);
}

void physmap_protect_arena_regions_noexecute() {
  const size_t num_arenas = pmm_num_arenas();
  fbl::AllocChecker ac;
  auto arenas = ktl::unique_ptr<pmm_arena_info_t[]>(new (&ac) pmm_arena_info_t[num_arenas]);
  ASSERT(ac.check());
  const size_t size = num_arenas * sizeof(pmm_arena_info_t);

  zx_status_t status = pmm_get_arena_info(num_arenas, 0, arenas.get(), size);
  ASSERT(status == ZX_OK);

  for (uint i = 0; i < num_arenas; i++) {
    physmap_modify_region(reinterpret_cast<vaddr_t>(paddr_to_physmap(arenas[i].base)),
                          /*size=*/arenas[i].size, /*mmu_flags=*/kPhysmapMmuFlags);
  }
}
