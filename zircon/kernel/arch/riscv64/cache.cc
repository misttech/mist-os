// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/cache.h>
#include <trace.h>

#include <kernel/mp.h>

#define LOCAL_TRACE 0

extern "C" {

// At the moment, none of these are implemented on hardware that is currently supported.
void arch_clean_cache_range(vaddr_t start, size_t len) {}
void arch_clean_invalidate_cache_range(vaddr_t start, size_t len) {}
void arch_invalidate_cache_range(vaddr_t start, size_t len) {}

// Synchronize the instruction and data cache on all cpus.
// Note: this is very expensive and requires cross cpu IPIs.
void arch_sync_cache_range(vaddr_t start, size_t len) {
  LTRACEF("start %#lx, len %zu\n", start, len);

  // Shootdown on all cores.  Using mp_sync_exec instead of an SBI remote fence to handle race
  // conditions with cores going offline.
  auto fencei = [](void*) { __asm__("fence.i"); };
  mp_sync_exec(MP_IPI_TARGET_ALL, /* cpu_mask */ 0, fencei, nullptr);
}

}  // extern C
