// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/x86/boot-cpuid.h>
#include <lib/memalloc/pool.h>
#include <lib/trivial-allocator/basic-leaky-allocator.h>
#include <lib/trivial-allocator/single-heap-allocator.h>
#include <stdio.h>
#include <zircon/limits.h>

#include <fbl/algorithm.h>
#include <hwreg/x86msr.h>
#include <ktl/array.h>
#include <ktl/byte.h>
#include <ktl/optional.h>
#include <ktl/span.h>
#include <phys/allocation.h>

#include "phys/address-space.h"

#include <ktl/enforce.h>

namespace {

// On x86-64, we don't have any guarantee that all the memory in our address
// space is actually mapped in.
//
// We use a bootstrap allocator consisting of memory from ".bss" to construct a
// real page table with.  Unused memory will be returned to the heap after
// initialisation is complete.
//
// Amount of memory reserved in .bss for allocation of page table data
// structures: We reserve 512kiB. On machines which only support at most 2 MiB
// page sizes, we need ~8 bytes per 2 MiB, allowing us to map ~128 GiB of
// RAM. On machines with 1 GiB page sizes, we can support ~64 TiB of RAM.
//
constexpr size_t kBootstrapMemoryBytes = 512 * 1024;

// Bootstrap memory pool.
alignas(ZX_MIN_PAGE_SIZE) ktl::array<ktl::byte, kBootstrapMemoryBytes> gBootstrapMemory;

void SetUpAddressSpace(AddressSpace::PageTableAllocator allocator) {
  // Ensure that executable pages are allowed.
  hwreg::X86MsrIo msr;
  arch::X86ExtendedFeatureEnableRegisterMsr::Get().ReadFrom(&msr).set_nxe(1).WriteTo(&msr);

  AddressSpace aspace;
  aspace.Init(ktl::move(allocator));
  aspace.SetUpIdentityMappings();
  aspace.ArchInstall();
}

}  // namespace

void ArchSetUpAddressSpaceEarly() {
  ktl::span<ktl::byte> memory{gBootstrapMemory};

  trivial_allocator::BasicLeakyAllocator<trivial_allocator::SingleHeapAllocator> allocator(memory);
  auto paging_allocator = [&allocator](uint64_t size,
                                       uint64_t alignment) -> ktl::optional<uint64_t> {
    void* allocated = allocator.allocate(size, alignment);
    if (!allocated) {
      return {};
    }
    return reinterpret_cast<uint64_t>(allocated);
  };
  SetUpAddressSpace(ktl::move(paging_allocator));

  ktl::span<const ktl::byte> leftover = allocator.unallocated();
  if (!leftover.empty()) {
    if (Allocation::GetPool()
            .Free(reinterpret_cast<uint64_t>(leftover.data()), leftover.size())
            .is_error()) {
      printf("Failed to release .bss bootstrap memory\n");
    }
  }
}
void ArchSetUpAddressSpaceLate() { SetUpAddressSpace(AddressSpace::PhysPageTableAllocator()); }
