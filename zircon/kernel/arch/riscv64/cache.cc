// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <lib/arch/cache.h>
#include <lib/arch/riscv64/feature.h>
#include <trace.h>

#include <arch/interrupt.h>
#include <arch/ops.h>
#include <arch/riscv64/feature.h>
#include <arch/riscv64/sbi.h>
#include <arch/vm.h>
#include <kernel/mp.h>

#define LOCAL_TRACE 0

namespace {

inline void cache_op(vaddr_t start, size_t len, void (*cbofunc)(vaddr_t)) {
  // If the Zicbom feature is enabled, use the cbo* instructions.
  // If there is no zicbom feature, the cpu is assumed to be coherent with
  // external DMA and not need any sort of cache flushing.
  if (gRiscvFeatures[arch::RiscvFeature::kZicbom]) {
    const size_t stride = riscv_cbom_size;
    const vaddr_t end = start + len;

    // Align the address. (stride must be a power of 2).
    start = ROUNDDOWN(start, stride);

    while (start < end) {
      cbofunc(start);
      start += stride;
    }
  }
}

}  // anonymous namespace

extern "C" {

// Using zicbom instructions, clean/invalidate/clean+invalidate the data cache
// over a range of memory.
void arch_clean_cache_range(vaddr_t start, size_t len) {
  LTRACEF("start %#lx, len %zu\n", start, len);

  // We can only flush kernel addresses, since user addresses may page fault.
  DEBUG_ASSERT(is_kernel_address(start));

  cache_op(start, len, [](vaddr_t addr) {
    __asm__ volatile("cbo.clean 0(%0)" ::"r"(addr) : "memory");
  });
}

void arch_clean_invalidate_cache_range(vaddr_t start, size_t len) {
  LTRACEF("start %#lx, len %zu\n", start, len);

  // We can only flush kernel addresses, since user addresses may page fault.
  DEBUG_ASSERT(is_kernel_address(start));

  cache_op(start, len, [](vaddr_t addr) {
    __asm__ volatile("cbo.flush 0(%0)" ::"r"(addr) : "memory");
  });
}

void arch_invalidate_cache_range(vaddr_t start, size_t len) {
  LTRACEF("start %#lx, len %zu\n", start, len);

  // We can only flush kernel addresses, since user addresses may page fault.
  DEBUG_ASSERT(is_kernel_address(start));

  cache_op(start, len, [](vaddr_t addr) {
    __asm__ volatile("cbo.inval 0(%0)" ::"r"(addr) : "memory");
  });
}

// Synchronize the instruction and data cache on all cpus.
// Note: this is very expensive and requires cross cpu IPIs.
void arch_sync_cache_range(vaddr_t start, size_t len) {
  LTRACEF("start %#lx, len %zu\n", start, len);

  // Shootdown on all cores.  Using mp_sync_exec instead of an SBI remote fence to handle race
  // conditions with cores going offline.
  auto fencei = [](void*) { __asm__ volatile("fence.i"); };
  mp_sync_exec(mp_ipi_target::ALL, /* cpu_mask */ 0, fencei, nullptr);
}

}  // extern C
