// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/paging.h>
#include <lib/arch/x86/system.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <zircon/limits.h>

#include <fbl/algorithm.h>
#include <ktl/algorithm.h>
#include <ktl/optional.h>
#include <phys/allocation.h>

#include "phys/address-space.h"

#include <ktl/enforce.h>

namespace {

void SwitchToPageTable(uint64_t root) {
  // Disable support for global pages ("page global enable"), which
  // otherwise would not be flushed in the operation below.
  arch::X86Cr4::Read().set_pge(0).Write();

  // Set the new page table root. This will flush the TLB.
  arch::X86Cr3::Write(root);
}

}  // namespace

void ArchSetUpIdentityAddressSpace(AddressSpace& aspace) {
  const auto& pool = Allocation::GetPool();
  uint64_t first = fbl::round_down(pool.front().addr, ZX_MAX_PAGE_SIZE);
  uint64_t last = fbl::round_up(pool.back().end(), ZX_MAX_PAGE_SIZE);
  ZX_DEBUG_ASSERT(first < last);
  auto result = aspace.IdentityMap(first, last - first,
                                   AddressSpace::NormalMapSettings({
                                       .readable = true,
                                       .writable = true,
                                       .executable = true,
                                   }));
  if (result.is_error()) {
    ZX_PANIC("Failed to map in range.");
  }

  // Switch to the new page table.
  SwitchToPageTable(aspace.root_paddr());
}
