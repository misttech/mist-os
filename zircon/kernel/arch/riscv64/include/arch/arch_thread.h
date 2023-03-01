// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ARCH_THREAD_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ARCH_THREAD_H_

#ifndef __ASSEMBLER__

#include <stdint.h>
#include <sys/types.h>
#include <zircon/tls.h>

struct arch_thread {
  // The compiler knows the position of these two fields relative to tp, which
  // is what __builtin_thread_pointer() returns.  i.e. to &abi[1].
  // tp points just past these.
  uintptr_t stack_guard;
  vaddr_t unsafe_sp;
  union {
    char thread_pointer_location;
    vaddr_t sp;
  };
};

#define thread_pointer_offsetof(field)        \
  ((int)offsetof(struct arch_thread, field) - \
   (int)offsetof(struct arch_thread, thread_pointer_location))

static_assert(thread_pointer_offsetof(stack_guard) == ZX_TLS_STACK_GUARD_OFFSET,
              "stack_guard field in wrong place");
static_assert(thread_pointer_offsetof(unsafe_sp) == ZX_TLS_UNSAFE_SP_OFFSET,
              "unsafe_sp field in wrong place");

#endif  // __ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ARCH_THREAD_H_
