// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <string.h>
#include <sys/types.h>

#include <arch/defines.h>
#include <arch/riscv64/mmu.h>
#include <fbl/algorithm.h>
#include <ktl/tuple.h>
#include <vm/physmap.h>

namespace {

// The structure of the RISC-V Page Table Entry (PTE) and the algorithm that is
// used is described in the RISC-V Privileged Spec in section 4.3.2, page 70.
//
// https://github.com/riscv/riscv-isa-manual/releases/download/Ratified-IMFDQC-and-Priv-v1.11/riscv-privileged-20190608.pdf
//
// Note that index levels in this code are reverse to the index levels found
// in the spec to make this code more consistent with other zircon platforms.
// In the RISC-V spec, for sv39, the top level of the page table tree is level 2
// whereas in the code below it is level 0.

// Level 0 PTEs may point to large pages of size 1GB.
constexpr uintptr_t l0_large_page_size = 1UL << (PAGE_SIZE_SHIFT + 2 * RISCV64_MMU_PT_SHIFT);
constexpr uintptr_t l0_large_page_size_mask = l0_large_page_size - 1;

// Level 1 PTEs may point to large pages of size 2MB.
constexpr uintptr_t l1_large_page_size = 1UL << (PAGE_SIZE_SHIFT + RISCV64_MMU_PT_SHIFT);
constexpr uintptr_t l1_large_page_size_mask = l1_large_page_size - 1;

// Physmap is mapped using L0 entries, each entry consist of 1 GB range.
constexpr size_t kPhysmapBootPages = 0;

constexpr size_t kKernelBootPages =
    // L1 pages for kernel mapping.
    fbl::round_up<size_t>(KERNEL_IMAGE_MAX_SIZE, l0_large_page_size) / l0_large_page_size +
    // L2 pages for kernel mapping.
    fbl::round_up<size_t>(KERNEL_IMAGE_MAX_SIZE, l1_large_page_size) / l1_large_page_size;

constexpr size_t kNumBootPageTables = kPhysmapBootPages + kKernelBootPages;
alignas(PAGE_SIZE) uint64_t boot_page_tables[RISCV64_MMU_PT_ENTRIES * kNumBootPageTables];

// Will track the physical address of the page table array above. Starts off initialized to the
// virtual address, but will be adjusted in riscv64_boot_map_init().
paddr_t boot_page_tables_pa = reinterpret_cast<paddr_t>(boot_page_tables);
size_t boot_page_table_offset;

// Extract the level 0 physical page number from the virtual address.  In the
// RISC-V spec this corresponds to va.vpn[2] for sv39.
constexpr size_t vaddr_to_l0_index(vaddr_t addr) {
  return (addr >> (PAGE_SIZE_SHIFT + 2 * RISCV64_MMU_PT_SHIFT)) & (RISCV64_MMU_PT_ENTRIES - 1);
}

// Extract the level 1 physical page number from the virtual address.  In the
// RISC-V spec this corresponds to va.vpn[1] for sv39.
constexpr size_t vaddr_to_l1_index(vaddr_t addr) {
  return (addr >> (PAGE_SIZE_SHIFT + RISCV64_MMU_PT_SHIFT)) & (RISCV64_MMU_PT_ENTRIES - 1);
}

// Extract the level 2 physical page number from the virtual address.  In the
// RISC-V spec this corresponds to va.vpn[0] for sv39.
constexpr size_t vaddr_to_l2_index(vaddr_t addr) {
  return (addr >> PAGE_SIZE_SHIFT) & (RISCV64_MMU_PT_ENTRIES - 1);
}

paddr_t boot_page_table_alloc() {
  ASSERT(boot_page_table_offset < sizeof(boot_page_tables));
  boot_page_table_offset += PAGE_SIZE;

  paddr_t new_table = boot_page_tables_pa;
  boot_page_tables_pa += PAGE_SIZE;
  return new_table;
}

}  // anonymous namespace

extern "C" void riscv64_boot_map_init(uint64_t vaddr_paddr_delta) {
  boot_page_tables_pa -= vaddr_paddr_delta;
}

std::tuple<size_t, size_t> riscv64_boot_map_used_memory() {
  return {sizeof(boot_page_tables), boot_page_table_offset};
}

// Early boot time page table creation code, called from start.S while running
// in physical address space with the mmu disabled. This code should be position
// independent as long as it sticks to basic code.

// Called from start.S to configure the page tables to map the kernel
// wherever it is located physically to KERNEL_BASE.  This function should not
// call functions itself since the full ABI it not set up yet.
extern "C" zx_status_t riscv64_boot_map(pte_t* kernel_ptable0, vaddr_t vaddr, paddr_t paddr,
                                        size_t len, const pte_t flags) {
  vaddr &= RISCV64_MMU_CANONICAL_MASK;

  // Loop through the virtual range and map each physical page using the largest
  // page size supported. Allocates necessary page tables along the way.

  while (len > 0) {
    size_t index0 = vaddr_to_l0_index(vaddr);
    pte_t* kernel_ptable1 = nullptr;
    if (!riscv64_pte_is_valid(kernel_ptable0[index0])) {
      // A large page can be used if both the virtual and physical addresses
      // are aligned to the large page size and the remaining amount of memory
      // to map is at least the large page size.
      bool can_map_large_file = ((vaddr & l0_large_page_size_mask) == 0) &&
                                ((paddr & l0_large_page_size_mask) == 0) &&
                                len >= l0_large_page_size;
      if (can_map_large_file) {
        kernel_ptable0[index0] = riscv64_pte_pa_to_pte((paddr & ~l0_large_page_size_mask)) | flags;
        vaddr += l0_large_page_size;
        paddr += l0_large_page_size;
        len -= l0_large_page_size;
        continue;
      }

      paddr_t pa = boot_page_table_alloc();
      kernel_ptable0[index0] = riscv64_pte_pa_to_pte(pa) | RISCV64_PTE_V;
      kernel_ptable1 = reinterpret_cast<pte_t*>(pa);
    } else if (!riscv64_pte_is_leaf(kernel_ptable0[index0])) {
      kernel_ptable1 = reinterpret_cast<pte_t*>(riscv64_pte_pa(kernel_ptable0[index0]));
    } else {
      return ZX_ERR_BAD_STATE;
    }

    // Setup level 1 PTE.
    size_t index1 = vaddr_to_l1_index(vaddr);
    pte_t* kernel_ptable2 = nullptr;
    if (!riscv64_pte_is_valid(kernel_ptable1[index1])) {
      // A large page can be used if both the virtual and physical addresses
      // are aligned to the large page size and the remaining amount of memory
      // to map is at least the large page size.
      bool can_map_large_file = ((vaddr & l1_large_page_size_mask) == 0) &&
                                ((paddr & l1_large_page_size_mask) == 0) &&
                                len >= l1_large_page_size;
      if (can_map_large_file) {
        kernel_ptable1[index1] = riscv64_pte_pa_to_pte((paddr & ~l1_large_page_size_mask)) | flags;
        vaddr += l1_large_page_size;
        paddr += l1_large_page_size;
        len -= l1_large_page_size;
        continue;
      }

      paddr_t pa = boot_page_table_alloc();
      kernel_ptable1[index1] = riscv64_pte_pa_to_pte(pa) | RISCV64_PTE_V;
      kernel_ptable2 = reinterpret_cast<pte_t*>(pa);
    } else if (!riscv64_pte_is_leaf(kernel_ptable1[index1])) {
      kernel_ptable2 = reinterpret_cast<pte_t*>(riscv64_pte_pa(kernel_ptable1[index1]));
    } else {
      return ZX_ERR_BAD_STATE;
    }

    // Setup level 2 PTE (always a leaf).
    size_t index2 = vaddr_to_l2_index(vaddr);
    kernel_ptable2[index2] = riscv64_pte_pa_to_pte(paddr) | flags;

    vaddr += PAGE_SIZE;
    paddr += PAGE_SIZE;
    len -= PAGE_SIZE;
  }

  return ZX_OK;
}
