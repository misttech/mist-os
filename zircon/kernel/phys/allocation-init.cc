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
  // kAllocationMinAddr is defined in arch-allocation.h.
  auto init_result = kAllocationMinAddr  // ktl::nullopt if don't care.
                         ? pool->Init(ranges, *kAllocationMinAddr)
                         : pool->Init(ranges);
  ZX_ASSERT(init_result.is_ok());

  // Install the pool for GetPool.
  InitWithPool(*pool);
}
