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

  ktl::array ranges{mem_ranges, special_ranges};
  // kArchAllocationMinAddr is defined in arch-allocation.h.
  if (kArchAllocationMinAddr) {
    ZX_ASSERT(pool->Init(ranges, *kArchAllocationMinAddr).is_ok());

    // While the pool will now prevent allocation in
    // [0, *kArchAllocationMinAddr), it can be strictly enforced by allocating
    // all such RAM now. Plus, it is convenient to distinguish this memory at
    // hand-off time.
    ZX_ASSERT(pool->UpdateFreeRamSubranges(memalloc::Type::kReservedLow, 0, *kArchAllocationMinAddr)
                  .is_ok());
  } else {
    ZX_ASSERT(pool->Init(ranges).is_ok());
  }

  // Install the pool for GetPool.
  InitWithPool(*pool);
}
