// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZIRCON_ABI_SPEC_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZIRCON_ABI_SPEC_H_

#include <stddef.h>
#include <stdint.h>
#include <zircon/assert.h>

// The kernel ABI specifications needed at the phys stage to properly prepare
// handoff.
//
// The expectation is that structure will be recorded as an ELF note in the
// kernel to be loaded and parsed during hand-off preparation.
//
// TODO(https://fxbug.dev/42164859): Sizes and alignments relating to C++ ABI
// set-up (e.g., stack sizes).
struct ZirconAbiSpec {
  struct Stack {
    template <size_t PageSize>
    constexpr void AssertValid() {
      ZX_ASSERT(size_bytes == (size_bytes & -PageSize));
      ZX_ASSERT(lower_guard_size_bytes == (lower_guard_size_bytes & -PageSize));
      ZX_ASSERT(upper_guard_size_bytes == (upper_guard_size_bytes & -PageSize));
    }

    // The size of the stack. Must be page-aligned.
    uint32_t size_bytes = 0;

    // The size of the unmapped 'guard' region to ensure lies below the mapped
    // stack. Must be page-aligned.
    uint32_t lower_guard_size_bytes = 0;

    // The size of the unmapped 'guard' region to ensure lies above the mapped
    // stack. Must be page-aligned.
    uint32_t upper_guard_size_bytes = 0;
  };

  template <size_t PageSize>
  constexpr void AssertValid() {
    machine_stack.AssertValid<PageSize>();
    shadow_call_stack.AssertValid<PageSize>();
  }

  Stack machine_stack;
  Stack shadow_call_stack;
};

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZIRCON_ABI_SPEC_H_
