// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MP_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MP_H_

#include <arch/defines.h>
#include <kernel/cpu.h>

struct percpu;

// Per cpu structure, pointed to by a fixed register while in kernel mode.
// Aligned on the maximum architectural cache line to avoid cache
// line sharing between CPUs.
struct alignas(MAX_CACHE_LINE) riscv64_percpu {
  // CPU number.
  cpu_num_t cpu_num;

  // Whether blocking is disallowed.  See arch_blocking_disallowed().
  uint32_t blocking_disallowed;

  // Number of spinlocks currently held.
  uint32_t num_spinlocks;

  // A pointer providing fast access to the high-level arch-agnostic per-CPU
  // struct.
  percpu* high_level_percpu;
};

// The compiler doesn't reliably generate the right code for setting the
// register via this variable, so it's only used for reading.  (Unfortunately
// it's not possible to declare it `const` to enforce this, since that's not
// compatible with an "uninitialized" definition, and a global register
// variable cannot have an initializer.)  Using this rather than inline asm for
// accesses via riscv64_read_percpu_ptr() lets the compiler optimize to direct
// load/store instructions using gp rather than copying it to a temporary
// register.  This can't be relied upon when it's important to use only a
// single instruction with risking a CPU switch via preemption (for those
// cases, it's necessary to use the READ_PERCPU_FIELD* and WRITE_PERCPU_FIELD*
// macros), but it gives the compiler the option.
register riscv64_percpu* riscv64_percpu_ptr __asm__("gp");

inline riscv64_percpu* riscv64_read_percpu_ptr() { return riscv64_percpu_ptr; }

// Mark as volatile to force a read of the field to make sure the compiler
// always emits a read when asked and does not cache a copy between.  For the
// same reason, this can't by done via the riscv64_percpu_ptr variable, since
// the compiler could copy gp into another register and access it after a
// reschedule.
template <typename T, size_t Offset>
[[gnu::always_inline]] inline T riscv64_read_percpu_field() {
  uint64_t value;
  __asm__ volatile("lwu %0, %1(gp)" : "=r"(value) : "I"(Offset));
  return static_cast<T>(value);
}
#define READ_PERCPU_FIELD32(field) \
  (riscv64_read_percpu_field<uint32_t, offsetof(riscv64_percpu, field)>())

template <typename T, size_t Offset>
[[gnu::always_inline]] inline void riscv64_write_percpu_field(T value) {
  __asm__ volatile("sw %0, %1(gp)" : : "r"(static_cast<uint64_t>(value)), "I"(Offset));
}
#define WRITE_PERCPU_FIELD32(field, value) \
  (riscv64_write_percpu_field<uint32_t, offsetof(riscv64_percpu, field)>(value))

// Return a pointer to the high-level percpu struct for the calling CPU.
inline percpu* arch_get_curr_percpu() { return riscv64_read_percpu_ptr()->high_level_percpu; }

inline cpu_num_t arch_curr_cpu_num() { return READ_PERCPU_FIELD32(cpu_num); }

// Setup the high-level percpu struct pointer for |cpu_num|.
void arch_setup_percpu(cpu_num_t cpu_num, percpu* percpu);

// TODO(travisg): implement
inline void arch_set_restricted_flag(bool restricted) {}

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MP_H_
