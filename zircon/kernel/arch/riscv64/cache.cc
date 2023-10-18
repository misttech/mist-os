// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/cache.h>
#include <trace.h>

#include <arch/interrupt.h>
#include <arch/ops.h>
#include <arch/riscv64/sbi.h>

#define LOCAL_TRACE 0

extern "C" {

// At the moment, none of these are implemented on hardware that is currently supported.
void arch_clean_cache_range(vaddr_t start, size_t len) {}
void arch_clean_invalidate_cache_range(vaddr_t start, size_t len) {}
void arch_invalidate_cache_range(vaddr_t start, size_t len) {}

// Synchronize the instruction and data cache on all cpus.
// Note: this is very expensive and requires a cross cpu interrupt via SBI.
void arch_sync_cache_range(vaddr_t start, size_t len) {
  LTRACEF("start %#lx, len %zu\n", start, len);

  // To keep the cpu from changing between the local shootdown and the remote,
  // disable interrupts around this operation.
  InterruptDisableGuard irqd;

  // Shootdown on the local core
  __asm__("fence.i");

  // Send the shootdown to the rest of the cores.
  const cpu_mask_t cpu_mask = mask_all_but_one(arch_curr_cpu_num());
  // TODO(maniscalco): Use an mp_sync_task instead of an SBI remote call to ensure that we properly
  // handle the case where one or more CPUs in the mask has transitioned to offline.  See also
  // fxbug.dev/135398.
  sbi_remote_fencei(cpu_mask);
}

}  // extern C
