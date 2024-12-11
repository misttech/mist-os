// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/memalloc/pool.h>

#include <fbl/no_destructor.h>
#include <ktl/array.h>
#include <phys/allocation.h>
#include <phys/arch/arch-allocation.h>

#include <ktl/enforce.h>

void Allocation::Init(ktl::span<memalloc::Range> mem_ranges,
                      ktl::span<memalloc::Range> special_ranges) {
  // Use fbl::NoDestructor to avoid generation of static destructors,
  // which fails in the phys environment.
  static fbl::NoDestructor<memalloc::Pool> pool;

  constexpr uint64_t kMin = kArchAllocationMinAddr.value_or(memalloc::Pool::kDefaultMinAddr);
  constexpr uint64_t kMax = kArchAllocationMaxAddr.value_or(memalloc::Pool::kDefaultMaxAddr);
  ktl::array ranges{mem_ranges, special_ranges};
  ZX_ASSERT(pool->Init(ranges, kMin, kMax).is_ok());

  if (kArchAllocationMinAddr) {
    // While the pool will now prevent allocation in
    // [0, *kArchAllocationMinAddr), it can be strictly enforced by allocating
    // all such RAM now. Plus, it is convenient to distinguish this memory at
    // hand-off time.
    ZX_ASSERT(pool->UpdateRamSubranges(memalloc::Type::kReservedLow, 0, kMin).is_ok());
  }

  // Install the pool for GetPool.
  InitWithPool(*pool);
}
