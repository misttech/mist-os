// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ARCH_INTERRUPT_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ARCH_INTERRUPT_H_

// Implementation of riscv64 specific routines to disable and reenable
// local interrupts on the current CPU.

#ifndef __ASSEMBLER__

#include <stdint.h>

#include <ktl/atomic.h>

using interrupt_saved_state_t = uint64_t;

[[nodiscard]] inline interrupt_saved_state_t arch_interrupt_save() {
  interrupt_saved_state_t old_state;
  __asm__ volatile("csrrw %0, sie, zero" : "=r"(old_state));
  if (old_state) {
    ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
  }
  return old_state;
}

static inline void arch_interrupt_restore(interrupt_saved_state_t old_state) {
  __asm__ volatile("csrw sie, %0" : : "Kr"(old_state));
}

inline void arch_disable_ints() {
  __asm__ volatile("csrw sie, zero" ::: "memory");
  ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
}

inline void arch_enable_ints() {
  constexpr uint64_t kAllInts = -1;
  ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
  __asm__ volatile("csrw sie, %0" : : "Kr"(kAllInts) : "memory");
}

inline bool arch_ints_disabled() {
  uint64_t sie;
  __asm__ volatile("csrr %0, sie" : "=r"(sie));
  return sie == 0;
}

#endif  // !__ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ARCH_INTERRUPT_H_
