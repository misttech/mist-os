// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/address-space.h"

#include <inttypes.h>
#include <lib/arch/paging.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/uart/uart.h>
#include <zircon/limits.h>
#include <zircon/types.h>

#include <fbl/algorithm.h>
#include <ktl/algorithm.h>
#include <ktl/byte.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/ref.h>
#include <ktl/type_traits.h>
#include <phys/stdio.h>
#include <phys/uart.h>

#include <ktl/enforce.h>

AddressSpace* gAddressSpace = nullptr;

void AddressSpace::AllocateRootPageTables() {
  // See allocator descriptions for the appropriate use-cases.
  auto lower_allocator = kDualSpaces ? temporary_allocator() : permanent_allocator();
  ktl::optional<uint64_t> lower_root = lower_allocator(
      LowerPaging::kTableSize<LowerPaging::kFirstLevel>, LowerPaging::kTableAlignment);
  ZX_ASSERT_MSG(lower_root, "failed to allocate %sroot page table", kDualSpaces ? "lower " : "");
  lower_root_paddr_ = *lower_root;

  if constexpr (kDualSpaces) {
    ktl::optional<uint64_t> upper_root = permanent_allocator()(
        UpperPaging::kTableSize<UpperPaging::kFirstLevel>, UpperPaging::kTableAlignment);
    ZX_ASSERT_MSG(upper_root, "failed to allocate upper root page table");
    upper_root_paddr_ = *upper_root;
  }
}

fit::result<AddressSpace::MapError> AddressSpace::Map(uint64_t vaddr, uint64_t size, uint64_t paddr,
                                                      AddressSpace::MapSettings settings) {
  ZX_ASSERT_MSG(vaddr < kLowerVirtualAddressRangeEnd || vaddr >= kUpperVirtualAddressRangeStart,
                "virtual address %#" PRIx64 " must be < %#" PRIx64 " or >= %#" PRIx64, vaddr,
                kLowerVirtualAddressRangeEnd, kUpperVirtualAddressRangeStart);

  bool upper = vaddr >= kUpperVirtualAddressRangeStart;

  // Fix-up settings per documented behavior.
  if constexpr (!kExecuteOnlyAllowed) {
    settings.access.readable |= !settings.access.writable && settings.access.executable;
  }

  settings.global = upper;

  if constexpr (kDualSpaces) {
    if (upper) {
      return UpperPaging::Map(upper_root_paddr_, paddr_to_io_, permanent_allocator(), state_, vaddr,
                              size, paddr, settings);
    }
  }

  // See allocator descriptions for the appropriate use-cases.
  auto lower_allocator = upper ? permanent_allocator() : temporary_allocator();
  return LowerPaging::Map(lower_root_paddr_, paddr_to_io_, lower_allocator, state_, vaddr, size,
                          paddr, settings);
}

void AddressSpace::IdentityMapRam() {
  memalloc::Pool& pool = Allocation::GetPool();

  // To account for the case of non-page-aligned RAM, we extend the mapped
  // region to page-aligned boundaries, tracking the end of the last aligned
  // range in the process. There should not be cases where both RAM and MMIO
  // appear within the same page.
  ktl::optional<uint64_t> last_aligned_end;
  memalloc::NormalizeRam(pool, [&](const memalloc::Range& range) {
    constexpr uint64_t kPageSize = ZX_PAGE_SIZE;

    // If the end of the last page-aligned range overlaps with the current,
    // take that to be the start of the current range.
    uint64_t addr, size;
    if (last_aligned_end && *last_aligned_end > range.addr) {
      if (*last_aligned_end >= range.end()) {
        return true;
      }
      addr = *last_aligned_end;
      size = range.end() - *last_aligned_end;
    } else {
      addr = range.addr & -kPageSize;
      size = (range.addr - addr) + range.size;
    }

    // Now page-align up the size.
    size = (size + kPageSize - 1) & ~(kPageSize - 1);

    auto result = IdentityMap(addr, size,
                              AddressSpace::NormalMapSettings({
                                  .readable = true,
                                  .writable = true,
                                  .executable = true,
                              }));
    if (result.is_error()) {
      ZX_PANIC("Failed to identity-map range [%#" PRIx64 ", %#" PRIx64
               ") (page-aligned from [%#" PRIx64 ", %#" PRIx64 "))",
               addr, addr + size, range.addr, range.end());
    }

    last_aligned_end = addr + size;
    return true;
  });
}

void AddressSpace::IdentityMapUart() {
  GetUartDriver().Visit([this]<typename KernelDriver>(const KernelDriver& driver) {
    if (ktl::optional<memalloc::Range> range = GetUartMmioRange(driver, ZX_PAGE_SIZE)) {
      auto result = IdentityMap(range->addr, range->size, MmioMapSettings());
      ZX_ASSERT_MSG(result.is_ok(), "Failed to map in UART range: [%#" PRIx64 ", %#" PRIx64 ")",
                    range->addr, range->size);
    }
  });
}
