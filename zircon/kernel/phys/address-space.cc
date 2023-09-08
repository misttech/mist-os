// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/arch/paging.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/uart/uart.h>
#include <zircon/limits.h>
#include <zircon/types.h>

#include <ktl/algorithm.h>
#include <ktl/byte.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/ref.h>
#include <ktl/type_traits.h>
#include <phys/address-space.h>
#include <phys/stdio.h>
#include <phys/uart.h>

#include <ktl/enforce.h>

AddressSpace::PageTableAllocator AddressSpace::PhysPageTableAllocator(memalloc::Pool& pool) {
  return [&pool](uint64_t alignment, uint64_t size) -> ktl::optional<uint64_t> {
    auto result = pool.Allocate(memalloc::Type::kIdentityPageTables, size, alignment);
    if (result.is_error()) {
      return ktl::nullopt;
    }
    uint64_t addr = ktl::move(result).value();
    memset(reinterpret_cast<void*>(addr), 0, static_cast<size_t>(size));
    return addr;
  };
}

fit::result<AddressSpace::MapError> AddressSpace::Map(uint64_t vaddr, uint64_t size, uint64_t paddr,
                                                      AddressSpace::MapSettings settings) {
  ZX_ASSERT_MSG(vaddr < kLowerVirtualAddressRangeEnd || vaddr >= kUpperVirtualAddressRangeStart,
                "virtual address %#" PRIx64 " must be < %#" PRIx64 " or >= %#" PRIx64, vaddr,
                kLowerVirtualAddressRangeEnd, kUpperVirtualAddressRangeStart);

  if constexpr (kDualSpaces) {
    if (vaddr >= kUpperVirtualAddressRangeStart) {
      return UpperPaging::Map(upper_root_paddr_, paddr_to_io_, ktl::ref(allocator_), state_, vaddr,
                              size, paddr, settings);
    }
  }
  return LowerPaging::Map(lower_root_paddr_, paddr_to_io_, ktl::ref(allocator_), state_, vaddr,
                          size, paddr, settings);
}

void AddressSpace::IdentityMapRam() {
  memalloc::Pool& pool = Allocation::GetPool();

  // To account for the case of non-page-aligned RAM, we extend the mapped
  // region to page-aligned boundaries, tracking the end of the last aligned
  // range in the process. There should not be cases where both RAM and MMIO
  // appear within the same page.
  ktl::optional<uint64_t> last_aligned_end;
  pool.NormalizeRam([&](const memalloc::Range& range) {
    constexpr uint64_t kPageSize = ZX_PAGE_SIZE;

    // If the end of the last page-aligned range overlaps with the current,
    // take that to be the start of the current range.
    uint64_t addr, size;
    if (last_aligned_end && *last_aligned_end > range.addr) {
      if (*last_aligned_end >= range.end()) {
        return;
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
  });
}

void AddressSpace::IdentityMapUart() {
  auto mapper = [this](uint64_t uart_mmio_base) -> volatile void* {
    uint64_t base = uart_mmio_base & ~(uint64_t{ZX_PAGE_SIZE} - 1);
    uint64_t size = ZX_PAGE_SIZE;
    auto result = IdentityMap(base, size, kMmioMapSettings);
    if (result.is_error()) {
      ZX_PANIC("Failed to map in UART range: [%#" PRIx64 ", %#" PRIx64 ")", uart_mmio_base,
               uart_mmio_base + ZX_PAGE_SIZE);
    }
    return reinterpret_cast<volatile void*>(uart_mmio_base);
  };

  GetUartDriver().Visit([mapper = ktl::move(mapper)](auto&& driver) {
    using config_type = typename ktl::decay_t<decltype(driver.uart())>::config_type;
    if constexpr (ktl::is_same_v<config_type, zbi_dcfg_simple_t>) {
      driver.io() = uart::BasicIoProvider<config_type>{
          driver.uart().config(),
          driver.uart().pio_size(),
          ktl::move(mapper),
      };
    }
    // Extend as more MMIO config types surface...
  });
}
