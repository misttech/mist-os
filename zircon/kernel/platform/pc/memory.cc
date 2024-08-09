// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "memory.h"

#include <assert.h>
#include <inttypes.h>
#include <lib/arch/x86/boot-cpuid.h>
#include <lib/memalloc/range.h>
#include <lib/zircon-internal/macros.h>
#include <platform.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/x86/bootstrap16.h>
#include <arch/x86/feature.h>
#include <arch/x86/mmu.h>
#include <dev/interrupt.h>
#include <efi/boot-services.h>
#include <fbl/algorithm.h>
#include <kernel/range_check.h>
#include <ktl/iterator.h>
#include <lk/init.h>
#include <object/handle.h>
#include <object/resource_dispatcher.h>
#include <phys/handoff.h>
#include <vm/vm.h>

#include "platform_p.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

// These are used to track memory arenas found during boot so they can
// be exclusively reserved within the resource system after the heap
// has been initialized.
constexpr uint8_t kMaxReservedMmioEntries = 64;
typedef struct reserved_mmio_space {
  uint64_t base;
  size_t len;
  KernelHandle<ResourceDispatcher> handle;
} reserved_mmio_space_t;
reserved_mmio_space_t reserved_mmio_entries[kMaxReservedMmioEntries];
static uint8_t reserved_mmio_count = 0;

constexpr uint8_t kMaxReservedPioEntries = 64;
typedef struct reserved_pio_space {
  uint64_t base;
  size_t len;
  KernelHandle<ResourceDispatcher> handle;
} reserved_pio_space_t;
reserved_pio_space_t reserved_pio_entries[kMaxReservedPioEntries];
static uint8_t reserved_pio_count = 0;

void mark_mmio_region_to_reserve(uint64_t base, size_t len) {
  ZX_DEBUG_ASSERT(reserved_mmio_count < kMaxReservedMmioEntries);
  reserved_mmio_entries[reserved_mmio_count].base = base;
  reserved_mmio_entries[reserved_mmio_count].len = len;
  reserved_mmio_count++;
}

void mark_pio_region_to_reserve(uint64_t base, size_t len) {
  ZX_DEBUG_ASSERT(reserved_pio_count < kMaxReservedPioEntries);
  reserved_pio_entries[reserved_pio_count].base = base;
  reserved_pio_entries[reserved_pio_count].len = len;
  reserved_pio_count++;
}

// Discover the basic memory map.
void pc_mem_init(ktl::span<const memalloc::Range> ranges) {
  pmm_checker_init_from_cmdline();

  // We don't want RAM under 1MiB to feature in to any arenas, so normalize that
  // here.
  ktl::span<const memalloc::Range> low_reserved;
  {
    size_t low_reserved_end = 0;
    while (low_reserved_end < ranges.size() &&
           ranges[low_reserved_end].type == memalloc::Type::kReservedLow) {
      ++low_reserved_end;
    }
    low_reserved = ranges.first(low_reserved_end);
    ranges = ranges.last(ranges.size() - low_reserved.size());
  }

  if (zx_status_t status = pmm_init(ranges); status != ZX_OK) {
    TRACEF("Error adding arenas from provided memory tables: error = %d\n", status);
  }

  auto filter_for_ram = [](memalloc::Type type) -> ktl::optional<memalloc::Type> {
    // Treat reserved test RAM as MMIO.
    if (type == memalloc::Type::kTestRamReserve || !memalloc::IsRamType(type)) {
      return {};
    }
    return memalloc::Type::kFreeRam;
  };
  memalloc::NormalizeRanges(
      ranges,
      [](const memalloc::Range& range) {
        mark_mmio_region_to_reserve(range.addr, static_cast<size_t>(range.size));
        return true;
      },
      filter_for_ram);

  // Find an area that we can use for 16 bit bootstrapping of other SMP cores.
  constexpr uint64_t kAllocSize = k_x86_bootstrap16_buffer_size;
  constexpr uint64_t kMinBase = 2UL * PAGE_SIZE;
  bool bootstrap16 = false;
  for (const memalloc::Range& range : low_reserved) {
    uint64_t base = ktl::max(range.addr, kMinBase);
    if (base >= range.end() || range.end() - base < kAllocSize) {
      continue;
    }

    // We have a valid range.
    LTRACEF("Selected %" PRIxPTR " as bootstrap16 region\n", base);
    x86_bootstrap16_init(base);
    bootstrap16 = true;
    break;
  }
  if (!bootstrap16) {
    TRACEF("WARNING - Failed to assign bootstrap16 region, SMP won't work\n");
  }
}

// Initialize the higher level PhysicalAspaceManager after the heap is initialized.
static void x86_resource_init_hook(unsigned int rl) {
  // An error is likely fatal if the bookkeeping is broken and driver
  ResourceDispatcher::InitializeAllocator(
      ZX_RSRC_KIND_MMIO, 0,
      (1ull << arch::BootCpuid<arch::CpuidAddressSizeInfo>().phys_addr_bits()) - 1);
  ResourceDispatcher::InitializeAllocator(ZX_RSRC_KIND_IOPORT, 0, UINT16_MAX);
  ResourceDispatcher::InitializeAllocator(ZX_RSRC_KIND_SYSTEM, 0, ZX_RSRC_SYSTEM_COUNT);

  // Exclusively reserve the regions marked as memory earlier so that physical
  // vmos cannot be created against them.
  for (uint8_t i = 0; i < reserved_mmio_count; i++) {
    zx_rights_t rights;
    auto& entry = reserved_mmio_entries[i];

    zx_status_t st =
        ResourceDispatcher::Create(&entry.handle, &rights, ZX_RSRC_KIND_MMIO, entry.base, entry.len,
                                   ZX_RSRC_FLAG_EXCLUSIVE, "platform_memory");
    if (st != ZX_OK) {
      TRACEF("failed to create backing resource for boot memory region %#lx - %#lx: %d\n",
             entry.base, entry.base + entry.len, st);
    }
  }

  // Exclusively reserve io ports in use
  for (uint8_t i = 0; i < reserved_pio_count; i++) {
    zx_rights_t rights;
    auto& entry = reserved_pio_entries[i];

    zx_status_t st =
        ResourceDispatcher::Create(&entry.handle, &rights, ZX_RSRC_KIND_IOPORT, entry.base,
                                   entry.len, ZX_RSRC_FLAG_EXCLUSIVE, "platform_io_port");
    if (st != ZX_OK) {
      TRACEF("failed to create backing resource for io port region %#lx - %#lx: %d\n", entry.base,
             entry.base + entry.len, st);
    }
  }

  // debug_uart.irq needs to be reserved here. See https://fxbug.dev/42109187.
}

LK_INIT_HOOK(x86_resource_init, x86_resource_init_hook, LK_INIT_LEVEL_HEAP)
