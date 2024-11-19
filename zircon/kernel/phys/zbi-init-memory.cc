// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/memalloc/pool.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/view.h>
#include <zircon/assert.h>

#include <ktl/array.h>
#include <ktl/limits.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/main.h>
#include <phys/symbolize.h>

#include <ktl/enforce.h>

void ZbiInitMemory(void* zbi, ktl::span<zbi_mem_range_t> mem_config,
                   ktl::optional<memalloc::Range> extra_special_range, AddressSpace* aspace) {
  zbitl::ByteView zbi_storage = zbitl::StorageFromRawHeader(static_cast<zbi_header_t*>(zbi));

  uint64_t phys_start = reinterpret_cast<uint64_t>(PHYS_LOAD_ADDRESS);
  uint64_t phys_end = reinterpret_cast<uint64_t>(_end);
  memalloc::Range special_memory_ranges[3] = {
      {
          .addr = phys_start,
          .size = phys_end - phys_start,
          .type = memalloc::Type::kPhysKernel,
      },
      {
          .addr = reinterpret_cast<uint64_t>(zbi_storage.data()),
          .size = zbi_storage.size_bytes(),
          .type = memalloc::Type::kDataZbi,
      },
  };

  ktl::span<memalloc::Range> zbi_ranges(memalloc::AsRanges(mem_config));
  ktl::span<memalloc::Range> special_ranges(special_memory_ranges);
  if (extra_special_range) {
    special_ranges.back() = *extra_special_range;
  } else {
    special_ranges = special_ranges.subspan(0, special_ranges.size() - 1);
  }

  Allocation::Init(zbi_ranges, special_ranges);

  // Set up our own address space.
  if (aspace) {
    ArchSetUpAddressSpace(*aspace);
  }

  // If there is a boot option to restrict the amount of available physical RAM,
  // apply it after we have the identity mapping in place for all actually
  // present RAM. Even if the pool forgets about certain regions, we might still
  // deliberately touch them (e.g., the ZBI or the pool's own bookkeeping).
  memalloc::Pool& pool = Allocation::GetPool();
  if (gBootOptions->memory_limit_mb > 0) {
    constexpr uint64_t kBytesPerMib = 0x100'000;
    uint64_t limit_mb = ktl::min(ktl::numeric_limits<uint64_t>::max() / kBytesPerMib,
                                 gBootOptions->memory_limit_mb);
    ZX_ASSERT(pool.TruncateTotalRam(kBytesPerMib * limit_mb).is_ok());
  }

  if (gBootOptions->phys_verbose) {
    pool.PrintMemoryRanges(ProgramName());
  }
}
