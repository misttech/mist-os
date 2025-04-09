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
#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <kernel/thread.h>
#include <ktl/array.h>
#include <phys/handoff.h>
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

namespace {

// The initialized VMARs described in the phys hand-off.
fbl::Vector<fbl::RefPtr<VmAddressRegion>> handoff_vmars;

constexpr uint32_t ToVmarFlags(PhysMapping::Permissions perms) {
  uint32_t flags = VMAR_FLAG_SPECIFIC | VMAR_FLAG_CAN_MAP_SPECIFIC;
  if (perms.readable()) {
    flags |= VMAR_FLAG_CAN_MAP_READ;
  }
  if (perms.writable()) {
    flags |= VMAR_FLAG_CAN_MAP_WRITE;
  }
  if (perms.executable()) {
    flags |= VMAR_FLAG_CAN_MAP_EXECUTE;
  }
  return flags;
}

constexpr uint ToArchMmuFlags(PhysMapping::Permissions perms) {
  uint flags = 0;
  if (perms.readable()) {
    flags |= ARCH_MMU_FLAG_PERM_READ;
  }
  if (perms.writable()) {
    flags |= ARCH_MMU_FLAG_PERM_WRITE;
  }
  if (perms.executable()) {
    flags |= ARCH_MMU_FLAG_PERM_EXECUTE;
  }
  return flags;
}

constexpr ktl::array<char, 4> PermissionsName(PhysMapping::Permissions perms) {
  ktl::array<char, 4> name{};
  size_t idx = 0;
  if (perms.readable()) {
    name[idx++] = 'r';
  }
  if (perms.writable()) {
    name[idx++] = 'w';
  }
  if (perms.executable()) {
    name[idx++] = 'x';
  }
  return name;
}

void RegisterMappings(ktl::span<const PhysMapping> mappings, fbl::RefPtr<VmAddressRegion> vmar) {
  for (const PhysMapping& mapping : mappings) {
    dprintf(ALWAYS,
            "VM: * mapping: %s (%s): [%#" PRIxPTR ", %#" PRIxPTR ") -> [%#" PRIxPTR ", %#" PRIxPTR
            ")\n",
            mapping.name.data(), PermissionsName(mapping.perms).data(), mapping.paddr,
            mapping.paddr_end(), mapping.vaddr, mapping.vaddr_end());
    zx_status_t status = vmar->ReserveSpace(mapping.name.data(), mapping.vaddr, mapping.size,
                                            ToArchMmuFlags(mapping.perms));
    ASSERT(status == ZX_OK);

#if __has_feature(address_sanitizer)
    if (mapping.kasan_shadow) {
      asan_map_shadow_for(mapping.vaddr, mapping.size);
    }
#endif  // __has_feature(address_sanitizer)
  }
}

fbl::RefPtr<VmAddressRegion> RegisterVmar(const PhysVmar& phys_vmar) {
  fbl::RefPtr<VmAddressRegion> root_vmar = VmAspace::kernel_aspace()->RootVmar();

  dprintf(ALWAYS, "VM: handing off VMAR from phys: %s @ [%#" PRIxPTR ", %#" PRIxPTR ")\n",
          phys_vmar.name.data(), phys_vmar.base, phys_vmar.end());
  fbl::RefPtr<VmAddressRegion> vmar;
  zx_status_t status =
      root_vmar->CreateSubVmar(phys_vmar.base - root_vmar->base(), phys_vmar.size, 0,
                               ToVmarFlags(phys_vmar.permissions()), phys_vmar.name.data(), &vmar);
  ASSERT(status == ZX_OK);
  RegisterMappings(phys_vmar.mappings.get(), vmar);

  return vmar;
}

}  // namespace

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

  fbl::AllocChecker ac;
  handoff_vmars.reserve(gPhysHandoff->vmars.size(), &ac);
  ASSERT(ac.check());
  for (const PhysVmar& phys_vmar : gPhysHandoff->vmars.get()) {
    fbl::RefPtr<VmAddressRegion> vmar = RegisterVmar(phys_vmar);
    handoff_vmars.push_back(ktl::move(vmar), &ac);
    ASSERT(ac.check());
  }

#ifndef __mist_os__
  // Protect the regions of the physmap that are not backed by normal memory.
  //
  // See the comments for |phsymap_protect_non_arena_regions| for why we're doing this.
  //
  physmap_protect_non_arena_regions();
#endif

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
