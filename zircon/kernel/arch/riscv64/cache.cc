// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/cache.h>

#include <arch/ops.h>

extern "C" {

// At the moment, none of these are implemented on hardware that is currently supported.
void arch_clean_cache_range(vaddr_t start, size_t len) {}
void arch_clean_invalidate_cache_range(vaddr_t start, size_t len) {}
void arch_invalidate_cache_range(vaddr_t start, size_t len) {}

void arch_sync_cache_range(vaddr_t start, size_t len) {
  // Dump all of the icache to synchronize with the dcache.
  arch::GlobalCacheConsistencyContext gccc;
  gccc.SyncRange(start, len);
}

}  // extern C
