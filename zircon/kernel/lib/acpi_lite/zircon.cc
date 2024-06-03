// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <inttypes.h>
#include <lib/acpi_lite.h>
#include <lib/acpi_lite/zircon.h>
#include <zircon/compiler.h>

#include <vm/physmap.h>
#include <vm/vm_aspace.h>

namespace {
// AcpiParser requires a ZirconPhysmemReader instance that outlives
// it. We share a single global instance for all AcpiParser instances.
acpi_lite::ZirconPhysmemReader g_physmem_reader;
}  // anonymous namespace

namespace acpi_lite {

zx::result<const void *> ZirconPhysmemReader::PhysToPtr(uintptr_t phys, size_t length) {
  // We don't support the 0 physical address or 0-length ranges.
  if (length == 0 || phys == 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Get the last byte of the specified range, ensuring we don't wrap around the address
  // space.
  uintptr_t phys_end;
  if (add_overflow(phys, length - 1, &phys_end)) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  // Ensure that both "phys" and "phys + length - 1" have valid addresses.
  //
  // The Zircon physmap is contiguous, so we don't have to worry about intermediate addresses.
  if (!is_physmap_phys_addr(phys) || !is_physmap_phys_addr(phys_end)) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  // Aim to map this 1-1 with what this address would be in the physmap. This essentially causes
  // pieces of the physmap that were not RAM, and may have previously been unmapped, to be mapped
  // back in if they are ACPI regions.
  const paddr_t paddr_base = ROUNDDOWN(phys, PAGE_SIZE);
  const vaddr_t vaddr_base = reinterpret_cast<vaddr_t>(paddr_to_physmap(paddr_base));
  if (vaddr_base == 0) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  ArchVmAspace &arch_aspace = VmAspace::kernel_aspace()->arch_aspace();

  paddr_t paddr = 0;
  uint mmu_flags = 0;
  if (arch_aspace.Query(vaddr_base, &paddr, &mmu_flags) == ZX_OK) {
    DEBUG_ASSERT(paddr == paddr_base);
    DEBUG_ASSERT((mmu_flags & ARCH_MMU_FLAG_PERM_READ) != 0);
    return zx::success(paddr_to_physmap(phys));
  }

  size_t mapped = 0;
  zx_status_t status = VmAspace::kernel_aspace()->arch_aspace().MapContiguous(
      vaddr_base, paddr_base, (ROUNDUP(phys_end, PAGE_SIZE) - paddr_base) / PAGE_SIZE,
      ARCH_MMU_FLAG_PERM_READ, &mapped);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::success(paddr_to_physmap(phys));
}

// Create a new AcpiParser, starting at the given Root System Description Pointer (RSDP).
zx::result<AcpiParser> AcpiParserInit(zx_paddr_t rsdp_pa) {
  return AcpiParser::Init(g_physmem_reader, rsdp_pa);
}

}  // namespace acpi_lite
