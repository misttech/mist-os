// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <string.h>
#include <sys/types.h>

#include <arch/arm64/mmu.h>
#include <arch/defines.h>
#include <fbl/algorithm.h>
#include <vm/physmap.h>
#include <vm/pmm.h>

// Early boot time page table creation code, called from start.S while running in physical address
// space with the mmu disabled. This code should be position independent as long as it sticks to
// basic code.
namespace {

// 1GB pages
const uintptr_t l1_large_page_size = 1UL << MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 1);
const uintptr_t l1_large_page_size_mask = l1_large_page_size - 1;

// 2MB pages
const uintptr_t l2_large_page_size = 1UL << MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 2);
const uintptr_t l2_large_page_size_mask = l2_large_page_size - 1;

// Uart range might not be contained in the 1G peripheral range.
// The peripheral ranges will:
// (1) Use a PTE from the L1 page allocated for the kernel mappings, hence
//     no L1 page allocation is needed.
// (2) The UART virtual range is below the kernel address, and within the same
//     GB, since it will always be the first periphmap range mapped.
constexpr size_t kPeripheralBootPages = 1;

// Just one L1 allocation for physmap.
constexpr size_t kPhysmapBootPages = 1;

// Boot pages needed for the kernel mapping.
constexpr size_t kKernelBootPages =
    // L1 page for kernel mapping.
    1 +
    // L2 pages for kernel mapping.
    fbl::round_up<size_t>(KERNEL_IMAGE_MAX_SIZE, l1_large_page_size) / l1_large_page_size +
    // L3 pages for kernel mapping.
    fbl::round_up<size_t>(KERNEL_IMAGE_MAX_SIZE, l2_large_page_size) / l2_large_page_size;

constexpr size_t kNumBootPageTables = kKernelBootPages + kPhysmapBootPages + kPeripheralBootPages;

alignas(PAGE_SIZE) uint64_t boot_page_tables[MMU_KERNEL_PAGE_TABLE_ENTRIES * kNumBootPageTables];

// Will track the physical address of the page table array above. Starts off initialized to the
// virtual address, but will be adjusted in arm64_boot_map_init().
paddr_t boot_page_tables_pa = reinterpret_cast<paddr_t>(boot_page_tables);
size_t boot_page_table_offset;

// this code only works on a 4K page granule, 48 bits of kernel address space
static_assert(MMU_KERNEL_PAGE_SIZE_SHIFT == 12);
static_assert(MMU_KERNEL_SIZE_SHIFT == 48);

size_t vaddr_to_l0_index(uintptr_t addr) {
  return (addr >> MMU_KERNEL_TOP_SHIFT) & (MMU_KERNEL_PAGE_TABLE_ENTRIES_TOP - 1);
}

size_t vaddr_to_l1_index(uintptr_t addr) {
  return (addr >> MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 1)) & (MMU_KERNEL_PAGE_TABLE_ENTRIES - 1);
}

size_t vaddr_to_l2_index(uintptr_t addr) {
  return (addr >> MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 2)) & (MMU_KERNEL_PAGE_TABLE_ENTRIES - 1);
}

size_t vaddr_to_l3_index(uintptr_t addr) {
  return (addr >> MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 3)) & (MMU_KERNEL_PAGE_TABLE_ENTRIES - 1);
}

// inner mapping routine passed two helper routines
__NO_SAFESTACK
inline zx_status_t _arm64_boot_map(pte_t* kernel_table0, const vaddr_t vaddr, const paddr_t paddr,
                                   const size_t len, const pte_t flags, paddr_t (*alloc_func)(),
                                   pte_t* phys_to_virt(paddr_t), const bool allow_large_pages) {
  // loop through the virtual range and map each physical page, using the largest
  // page size supported. Allocates necessary page tables along the way.
  size_t off = 0;
  while (off < len) {
    // make sure the level 1 pointer is valid
    size_t index0 = vaddr_to_l0_index(vaddr + off);
    pte_t* kernel_table1 = nullptr;
    switch (kernel_table0[index0] & MMU_PTE_DESCRIPTOR_MASK) {
      default: {  // invalid/unused entry
        paddr_t pa = alloc_func();
        kernel_table0[index0] = (pa & MMU_PTE_OUTPUT_ADDR_MASK) | MMU_PTE_L012_DESCRIPTOR_TABLE;
        __FALLTHROUGH;
      }
      case MMU_PTE_L012_DESCRIPTOR_TABLE:
        kernel_table1 = phys_to_virt(kernel_table0[index0] & MMU_PTE_OUTPUT_ADDR_MASK);
        break;
      case MMU_PTE_L012_DESCRIPTOR_BLOCK:
        // not legal to have a block pointer at this level
        return ZX_ERR_BAD_STATE;
    }

    // make sure the level 2 pointer is valid
    size_t index1 = vaddr_to_l1_index(vaddr + off);
    pte_t* kernel_table2 = nullptr;
    switch (kernel_table1[index1] & MMU_PTE_DESCRIPTOR_MASK) {
      default: {  // invalid/unused entry
        // a large page at this level is 1GB long, see if we can make one here
        if (allow_large_pages && (((vaddr + off) & l1_large_page_size_mask) == 0) &&
            (((paddr + off) & l1_large_page_size_mask) == 0) && (len - off) >= l1_large_page_size) {
          // set up a 1GB page here
          kernel_table1[index1] =
              ((paddr + off) & ~l1_large_page_size_mask) | flags | MMU_PTE_L012_DESCRIPTOR_BLOCK;

          off += l1_large_page_size;
          continue;
        }

        paddr_t pa = alloc_func();
        kernel_table1[index1] = (pa & MMU_PTE_OUTPUT_ADDR_MASK) | MMU_PTE_L012_DESCRIPTOR_TABLE;
        __FALLTHROUGH;
      }
      case MMU_PTE_L012_DESCRIPTOR_TABLE:
        kernel_table2 = phys_to_virt(kernel_table1[index1] & MMU_PTE_OUTPUT_ADDR_MASK);
        break;
      case MMU_PTE_L012_DESCRIPTOR_BLOCK:
        // not legal to have a block pointer at this level
        return ZX_ERR_BAD_STATE;
    }

    // make sure the level 3 pointer is valid
    size_t index2 = vaddr_to_l2_index(vaddr + off);
    pte_t* kernel_table3 = nullptr;
    switch (kernel_table2[index2] & MMU_PTE_DESCRIPTOR_MASK) {
      default: {  // invalid/unused entry
        // a large page at this level is 2MB long, see if we can make one here
        if (allow_large_pages && (((vaddr + off) & l2_large_page_size_mask) == 0) &&
            (((paddr + off) & l2_large_page_size_mask) == 0) && (len - off) >= l2_large_page_size) {
          // set up a 2MB page here
          kernel_table2[index2] =
              ((paddr + off) & ~l2_large_page_size_mask) | flags | MMU_PTE_L012_DESCRIPTOR_BLOCK;

          off += l2_large_page_size;
          continue;
        }

        paddr_t pa = alloc_func();

        kernel_table2[index2] = (pa & MMU_PTE_OUTPUT_ADDR_MASK) | MMU_PTE_L012_DESCRIPTOR_TABLE;
        __FALLTHROUGH;
      }
      case MMU_PTE_L012_DESCRIPTOR_TABLE:
        kernel_table3 = phys_to_virt(kernel_table2[index2] & MMU_PTE_OUTPUT_ADDR_MASK);
        break;
      case MMU_PTE_L012_DESCRIPTOR_BLOCK:
        // not legal to have a block pointer at this level
        return ZX_ERR_BAD_STATE;
    }

    // generate a standard page mapping
    size_t index3 = vaddr_to_l3_index(vaddr + off);
    kernel_table3[index3] =
        ((paddr + off) & MMU_PTE_OUTPUT_ADDR_MASK) | flags | MMU_PTE_L3_DESCRIPTOR_PAGE;

    off += PAGE_SIZE;
  }

  return ZX_OK;
}

__NO_SAFESTACK
paddr_t boot_page_table_alloc() {
  ASSERT(boot_page_table_offset < sizeof(boot_page_tables));
  boot_page_table_offset += PAGE_SIZE;
  paddr_t new_table = boot_page_tables_pa;
  boot_page_tables_pa += PAGE_SIZE;
  return new_table;
}

}  // anonymous namespace

__NO_SAFESTACK
extern "C" void arm64_boot_map_init(uint64_t vaddr_paddr_delta) {
  boot_page_tables_pa -= vaddr_paddr_delta;
}

std::tuple<size_t, size_t> arm64_boot_map_used_memory() {
  return {sizeof(boot_page_tables), boot_page_table_offset};
}

// called from start.S to configure level 1-3 page tables to map the kernel wherever it is located
// physically to KERNEL_BASE
__NO_SAFESTACK
extern "C" zx_status_t arm64_boot_map(pte_t* kernel_table0, const vaddr_t vaddr,
                                      const paddr_t paddr, const size_t len, const pte_t flags,
                                      bool allow_large_pages) {
  // the following helper routines assume that code is running in physical addressing mode (mmu
  // off). any physical addresses calculated are assumed to be the same as virtual
  auto alloc = []() __NO_SAFESTACK -> paddr_t {
    // allocate a page out of the boot allocator, asking for a physical address
    paddr_t pa = boot_page_table_alloc();

    // avoid using memset, since this relies on dc zva instruction, which isn't set up at
    // this point in the boot process
    // use a volatile pointer to make sure the compiler doesn't emit a memset call
    volatile pte_t* vptr = reinterpret_cast<volatile pte_t*>(pa);
    for (auto i = 0; i < MMU_KERNEL_PAGE_TABLE_ENTRIES; i++) {
      vptr[i] = 0;
    }

    return pa;
  };

  auto phys_to_virt = [](paddr_t pa)
                          __NO_SAFESTACK -> pte_t* { return reinterpret_cast<pte_t*>(pa); };

  return _arm64_boot_map(kernel_table0, vaddr, paddr, len, flags, alloc, phys_to_virt,
                         allow_large_pages);
}

// called a bit later in the boot process once the kernel is in virtual memory to map early kernel
// data
zx_status_t arm64_boot_map_v(const vaddr_t vaddr, const paddr_t paddr, const size_t len,
                             const pte_t flags, bool allow_large_pages) {
  // assumed to be running with virtual memory enabled, so use a slightly different set of routines
  // to allocate and find the virtual mapping of memory
  auto alloc = []() -> paddr_t {
    // allocate a page out of the boot allocator, asking for a physical address
    paddr_t pa = boot_page_table_alloc();
    // zero the memory using the physmap
    void* ptr = paddr_to_physmap(pa);
    memset(ptr, 0, MMU_KERNEL_PAGE_TABLE_ENTRIES * sizeof(pte_t));

    return pa;
  };

  auto phys_to_virt = [](paddr_t pa) -> pte_t* {
    return reinterpret_cast<pte_t*>(paddr_to_physmap(pa));
  };

  return _arm64_boot_map(arm64_get_kernel_ptable(), vaddr, paddr, len, flags, alloc, phys_to_virt,
                         allow_large_pages);
}

// Walk all the page tables allocated in the boot process and move them from the WIRED to the MMU
// state and correctly set their mapping count. This will allow post boot arch aspace modifications
// that expect a valid mapping count to behave correctly.
void arm64_boot_mmu_unwire() {
  const size_t num_pages = boot_page_table_offset / PAGE_SIZE;
  const paddr_t paddr_base = boot_page_tables_pa - boot_page_table_offset;

  auto process_page = [](paddr_t paddr) {
    const pte_t* table = reinterpret_cast<const pte_t*>(paddr_to_physmap(paddr));
    vm_page_t* page = Pmm::Node().PaddrToPage(paddr);
    DEBUG_ASSERT(page);
    DEBUG_ASSERT(page->state() == vm_page_state::WIRED);

    uint count = 0;
    for (size_t j = 0; j < MMU_KERNEL_PAGE_TABLE_ENTRIES; j++) {
      if ((table[j] & MMU_PTE_DESCRIPTOR_MASK) != MMU_PTE_DESCRIPTOR_INVALID) {
        count++;
      }
    }
    page->set_state(vm_page_state::MMU);
    page->mmu.num_mappings = count;
  };

  for (size_t i = 0; i < num_pages; i++) {
    const paddr_t paddr = paddr_base + i * PAGE_SIZE;
    process_page(paddr);
  }
  process_page(arm64_get_kernel_ptable_phys());
}
