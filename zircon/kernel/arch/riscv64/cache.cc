// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/ops.h>

// TODO-rvbringup: fill these in

// Unimplemented cache operations.
extern "C" {
void arch_clean_cache_range(vaddr_t start, size_t len) { PANIC_UNIMPLEMENTED; }
void arch_clean_invalidate_cache_range(vaddr_t start, size_t len) { PANIC_UNIMPLEMENTED; }
void arch_invalidate_cache_range(vaddr_t start, size_t len) { PANIC_UNIMPLEMENTED; }
void arch_sync_cache_range(vaddr_t start, size_t len) { PANIC_UNIMPLEMENTED; }
}  // extern C
