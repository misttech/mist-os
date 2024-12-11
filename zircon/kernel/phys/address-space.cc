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

  // TODO(https://fxbug.dev/42164859): Hack! Some of the identity map page
  // tables are reused on x86 to map the kernel to [-4GiB, -3GiB). So mark
  // everything as global for now. The kernel proper does CR4 reset when
  // unmapping the identity map anyway.
#ifdef __x86_64__
  settings.global = true;
#else
  settings.global = upper;
#endif

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
  auto mapper = [this](uint64_t uart_mmio_base, size_t uart_mmio_size) -> volatile void* {
    // Page aligned base and size.
    uint64_t base = uart_mmio_base & ~(uint64_t{ZX_PAGE_SIZE} - 1);
    uint64_t size = fbl::round_up(uart_mmio_base + uart_mmio_size, ZX_PAGE_SIZE) - base;
    auto result = IdentityMap(base, size, MmioMapSettings());
    if (result.is_error()) {
      ZX_PANIC("Failed to map in UART range: [%#" PRIx64 ", %#" PRIx64 ")", uart_mmio_base,
               uart_mmio_base + size);
    }
    return reinterpret_cast<volatile void*>(uart_mmio_base);
  };

  GetUartDriver().Visit([mapper = ktl::move(mapper)](auto&& driver) {
    using uart_type = ktl::decay_t<decltype(driver.uart())>;
    using config_type = typename uart_type::config_type;
    if constexpr (ktl::is_same_v<config_type, zbi_dcfg_simple_t>) {
      driver.io() = uart::BasicIoProvider<config_type, uart_type::kIoType>{
          driver.uart().config(),
          driver.uart().io_slots(),
          ktl::move(mapper),
      };
    }
    // Extend as more MMIO config types surface...
  });
}
