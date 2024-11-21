// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2014 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "vm/vm.h"

#include <align.h>
#include <assert.h>
#include <debug.h>
#include <inttypes.h>
#include <lib/boot-options/boot-options.h>
#include <lib/cmpctmalloc.h>
#include <lib/console.h>
#include <lib/crypto/global_prng.h>
#include <lib/instrumentation/asan.h>
#include <lib/lazy_init/lazy_init.h>
#include <lib/zircon-internal/macros.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/algorithm.h>
#include <kernel/thread.h>
#include <ktl/array.h>
#include <vm/init.h>
#include <vm/physmap.h>
#include <vm/pmm.h>
#include <vm/vm_address_region.h>
#include <vm/vm_aspace.h>
#include <vm/vm_object_paged.h>

#include "vm_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE VM_GLOBAL_TRACE(0)

// boot time allocated page full of zeros
vm_page_t* zero_page;
paddr_t zero_page_paddr;

// set early in arch code to record the start address of the kernel
paddr_t kernel_base_phys;

// construct an array of kernel program segment descriptors for use here
// and elsewhere
namespace {

const ktl::array _kernel_regions = {
    kernel_region{
        .name = "kernel_code",
        .base = (vaddr_t)__code_start,
        .size = ROUNDUP((uintptr_t)__code_end - (uintptr_t)__code_start, PAGE_SIZE),
        .arch_mmu_flags = ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_EXECUTE,
    },
    kernel_region{
        .name = "kernel_rodata",
        .base = (vaddr_t)__rodata_start,
        .size = ROUNDUP((uintptr_t)__rodata_end - (uintptr_t)__rodata_start, PAGE_SIZE),
        .arch_mmu_flags = ARCH_MMU_FLAG_PERM_READ,
    },
    kernel_region{
        .name = "kernel_relro",
        .base = (vaddr_t)__relro_start,
        .size = ROUNDUP((uintptr_t)__relro_end - (uintptr_t)__relro_start, PAGE_SIZE),
        .arch_mmu_flags = ARCH_MMU_FLAG_PERM_READ,
    },
    kernel_region{
        .name = "kernel_data_bss",
        .base = (vaddr_t)__data_start,
        .size = ROUNDUP((uintptr_t)_end - (uintptr_t)__data_start, PAGE_SIZE),
        .arch_mmu_flags = ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE,
    },
};

// The VMAR containing the kernel's load image and the physmap, respectively.
fbl::RefPtr<VmAddressRegion> kernel_image_vmar, physmap_vmar;

}  // namespace

const ktl::span<const kernel_region> kernel_regions{_kernel_regions};

void vm_init_preheap() {}

// Global so that it can be friended by VmAddressRegion.
void vm_init() {
  LTRACE_ENTRY;

  // Initialize VMM data structures.
  VmAspace::KernelAspaceInit();

  // grab a page and mark it as the zero page
  zx_status_t status = pmm_alloc_page(0, &zero_page, &zero_page_paddr);
  DEBUG_ASSERT(status == ZX_OK);

  // consider the zero page a wired page part of the kernel.
  zero_page->set_state(vm_page_state::WIRED);

  void* ptr = paddr_to_physmap(zero_page_paddr);
  DEBUG_ASSERT(ptr);

  arch_zero_page(ptr);

  fbl::RefPtr<VmAddressRegion> root_vmar = VmAspace::kernel_aspace()->RootVmar();

  // Full RWX permissions, to be refined by individual kernel regions.
  constexpr uint32_t kKernelVmarFlags =
      VMAR_FLAG_SPECIFIC | VMAR_FLAG_CAN_MAP_SPECIFIC | VMAR_CAN_RWX_FLAGS;

  // |kernel_image_size| is the size in bytes of the region of memory occupied by the kernel
  // program's various segments (code, rodata, data, bss, etc.), inclusive of any gaps between
  // them.
  const size_t kernel_image_size = get_kernel_size();
  const uintptr_t kernel_vaddr = reinterpret_cast<uintptr_t>(__executable_start);
  status = root_vmar->CreateSubVmar(kernel_vaddr - root_vmar->base(), kernel_image_size, 0,
                                    kKernelVmarFlags, "kernel region vmar", &kernel_image_vmar);
  ASSERT(status == ZX_OK);

  // Finish reserving the sections in the kernel_region
  for (const auto& region : kernel_regions) {
    if (region.size == 0) {
      continue;
    }

    ASSERT(IS_PAGE_ALIGNED(region.base));

    dprintf(ALWAYS,
            "VM: reserving kernel region [%#" PRIxPTR ", %#" PRIxPTR ") flags %#x name '%s'\n",
            region.base, region.base + region.size, region.arch_mmu_flags, region.name);
    status = kernel_image_vmar->ReserveSpace(region.name, region.base, region.size,
                                             region.arch_mmu_flags);
    ASSERT(status == ZX_OK);

#if __has_feature(address_sanitizer)
    asan_map_shadow_for(region.base, region.size);
#endif  // __has_feature(address_sanitizer)
  }

  constexpr uint32_t kPhysmapVmarFlags = VMAR_FLAG_SPECIFIC | VMAR_FLAG_CAN_MAP_SPECIFIC |
                                         VMAR_FLAG_CAN_MAP_READ | VMAR_FLAG_CAN_MAP_WRITE;
  status = root_vmar->CreateSubVmar(PHYSMAP_BASE - root_vmar->base(), PHYSMAP_SIZE, 0,
                                    kPhysmapVmarFlags, "physmap", &physmap_vmar);
  ASSERT(status == ZX_OK);

  // Protect the regions of the physmap that are not backed by normal memory.
  //
  // See the comments for |phsymap_protect_non_arena_regions| for why we're doing this.
  //
  physmap_protect_non_arena_regions();

  // Mark the physmap no-execute.
  physmap_protect_arena_regions_noexecute();

  cmpct_set_fill_on_alloc_threshold(gBootOptions->alloc_fill_threshold);
}

paddr_t vaddr_to_paddr(const void* va) {
  if (is_physmap_addr(va)) {
    return physmap_to_paddr(va);
  }

  // It doesn't make sense to be calling this on a non-kernel address, since we would otherwise be
  // querying some 'random' active user address space, which is unlikely to be what the caller
  // wants.
  if (!is_kernel_address(reinterpret_cast<vaddr_t>(va))) {
    return 0;
  }

  paddr_t pa;
  zx_status_t rc =
      VmAspace::kernel_aspace()->arch_aspace().Query(reinterpret_cast<vaddr_t>(va), &pa, nullptr);
  if (rc != ZX_OK) {
    return 0;
  }

  return pa;
}

static int cmd_vm(int argc, const cmd_args* argv, uint32_t) {
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments\n");
  usage:
    printf("usage:\n");
    printf("%s phys2virt <address>\n", argv[0].str);
    printf("%s virt2phys <address>\n", argv[0].str);
    printf("%s map <phys> <virt> <count> <flags>\n", argv[0].str);
    printf("%s unmap <virt> <count>\n", argv[0].str);
    return ZX_ERR_INTERNAL;
  }

  if (!strcmp(argv[1].str, "phys2virt")) {
    if (argc < 3) {
      goto notenoughargs;
    }

    if (!is_physmap_phys_addr(argv[2].u)) {
      printf("address isn't in physmap\n");
      return -1;
    }

    void* ptr = paddr_to_physmap((paddr_t)argv[2].u);
    printf("paddr_to_physmap returns %p\n", ptr);
  } else if (!strcmp(argv[1].str, "virt2phys")) {
    if (argc < 3) {
      goto notenoughargs;
    }

    if (!is_kernel_address(reinterpret_cast<vaddr_t>(argv[2].u))) {
      printf("ERROR: outside of kernel address space\n");
      return -1;
    }

    paddr_t pa;
    uint flags;
    zx_status_t err = VmAspace::kernel_aspace()->arch_aspace().Query(argv[2].u, &pa, &flags);
    printf("arch_mmu_query returns %d\n", err);
    if (err >= 0) {
      printf("\tpa %#" PRIxPTR ", flags %#x\n", pa, flags);
    }
  } else if (!strcmp(argv[1].str, "map")) {
    if (argc < 6) {
      goto notenoughargs;
    }

    if (!is_kernel_address(reinterpret_cast<vaddr_t>(argv[3].u))) {
      printf("ERROR: outside of kernel address space\n");
      return -1;
    }

    size_t mapped;
    auto err = VmAspace::kernel_aspace()->arch_aspace().MapContiguous(
        argv[3].u, argv[2].u, (uint)argv[4].u, (uint)argv[5].u, &mapped);
    printf("arch_mmu_map returns %d, mapped %zu\n", err, mapped);
  } else if (!strcmp(argv[1].str, "unmap")) {
    if (argc < 4) {
      goto notenoughargs;
    }

    if (!is_kernel_address(reinterpret_cast<vaddr_t>(argv[2].u))) {
      printf("ERROR: outside of kernel address space\n");
      return -1;
    }

    size_t unmapped;
    // Strictly only attempt to unmap exactly what the user requested, they can deal with any
    // failure that might result.
    auto err = VmAspace::kernel_aspace()->arch_aspace().Unmap(
        argv[2].u, (uint)argv[3].u, ArchVmAspace::EnlargeOperation::No, &unmapped);
    printf("arch_mmu_unmap returns %d, unmapped %zu\n", err, unmapped);
  } else {
    printf("unknown command\n");
    goto usage;
  }

  return ZX_OK;
}

STATIC_COMMAND_START
STATIC_COMMAND("vm", "vm commands", &cmd_vm)
STATIC_COMMAND_END(vm)
