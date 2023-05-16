// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MP_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MP_H_

#define PERCPU_IN_RESTRICTED_MODE 24

#ifndef __ASSEMBLER__

#include <lib/arch/riscv64/sbi.h>
#include <zircon/compiler.h>

#include <arch/defines.h>
#include <arch/riscv64.h>
#include <kernel/align.h>
#include <kernel/cpu.h>
#include <ktl/atomic.h>

struct percpu;

// Per cpu structure, pointed to by a fixed register while in kernel mode.
// Aligned on the maximum architectural cache line to avoid cache
// line sharing between CPUs.
struct alignas(MAX_CACHE_LINE) riscv64_percpu {
  // CPU number.
  cpu_num_t cpu_num;

  // The hart id is used by other components (SBI/PLIC etc...)
  uint32_t hart_id;

  // Whether blocking is disallowed.  See arch_blocking_disallowed().
  uint32_t blocking_disallowed;

  // Number of spinlocks currently held.
  uint32_t num_spinlocks;

  // A pointer providing fast access to the high-level arch-agnostic per-CPU
  // struct.
  percpu* high_level_percpu;

  // Flag to track that we're in restricted mode.
  uint32_t in_restricted_mode;

  // A bitmask of queued ipis for this cpu on its own cache line to avoid
  // aliasing with the rest of the percpu data since this is frequently accessed
  // from external cpus.
  __CPU_ALIGN ktl::atomic<uint32_t> ipi_data;
};
static_assert(offsetof(struct riscv64_percpu, in_restricted_mode) == PERCPU_IN_RESTRICTED_MODE,
              "in_restricted mode is at the wrong offset");

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
register riscv64_percpu* riscv64_percpu_ptr __asm__("s11");

inline void riscv64_set_percpu(struct riscv64_percpu* ptr) {
  __asm__ volatile("mv s11, %0" ::"r"(ptr), "m"(*ptr));
}

inline riscv64_percpu* riscv64_read_percpu_ptr() { return riscv64_percpu_ptr; }

// Mark as volatile to force a read of the field to make sure the compiler
// always emits a read when asked and does not cache a copy between.  For the
// same reason, this can't by done via the riscv64_percpu_ptr variable, since
// the compiler could copy s11 into another register and access it after a
// reschedule.
template <typename T, size_t Offset>
[[gnu::always_inline]] inline T riscv64_read_percpu_field() {
  if constexpr (sizeof(T) == sizeof(uint32_t)) {
    uint32_t value;
    __asm__ volatile("lwu %0, %1(s11)" : "=r"(value) : "I"(Offset));
    return static_cast<T>(value);
  } else {
    static_assert(sizeof(T) == sizeof(uint64_t));
    uint64_t value;
    __asm__ volatile("ld %0, %1(s11)" : "=r"(value) : "I"(Offset));
    return static_cast<T>(value);
  }
}
#define READ_PERCPU_FIELD32(field) \
  (riscv64_read_percpu_field<uint32_t, offsetof(riscv64_percpu, field)>())

template <typename T, size_t Offset>
[[gnu::always_inline]] inline void riscv64_write_percpu_field(T value) {
  if constexpr (sizeof(T) == sizeof(uint32_t)) {
    __asm__ volatile("sw %0, %1(s11)" : : "r"(static_cast<uint32_t>(value)), "I"(Offset));
  } else {
    static_assert(sizeof(T) == sizeof(uint64_t));
    __asm__ volatile("sd %0, %1(s11)" : : "r"(static_cast<uint64_t>(value)), "I"(Offset));
  }
}
#define WRITE_PERCPU_FIELD32(field, value) \
  (riscv64_write_percpu_field<uint32_t, offsetof(riscv64_percpu, field)>(value))

// Setup the high-level percpu struct pointer for |cpu_num|.
void arch_setup_percpu(cpu_num_t cpu_num, percpu* percpu);

// Return a pointer to the high-level percpu struct for the calling CPU.
inline struct percpu* arch_get_curr_percpu() {
  return riscv64_read_percpu_ptr()->high_level_percpu;
}

extern uint riscv64_num_cpus;

// This needs to be set very early (before arch_init).
inline void arch_set_num_cpus(uint cpu_count) { riscv64_num_cpus = cpu_count; }
inline uint arch_max_num_cpus() { return riscv64_num_cpus; }

void riscv64_mp_early_init_percpu(uint32_t hart_id, uint cpu_num);

inline cpu_num_t arch_curr_cpu_num() { return READ_PERCPU_FIELD32(cpu_num); }
inline uint32_t riscv64_curr_hart_id() { return READ_PERCPU_FIELD32(hart_id); }

// Translate a bitmap of cpu ids to a bitmap of harts, which may not be 1:1
arch::HartMask riscv64_cpu_mask_to_hart_mask(cpu_mask_t cmask);

inline bool arch_get_restricted_flag() { return READ_PERCPU_FIELD32(in_restricted_mode); }
inline void arch_set_restricted_flag(bool restricted) {
  WRITE_PERCPU_FIELD32(in_restricted_mode, restricted ? 1 : 0);
}

uint32_t riscv64_boot_hart_id();
zx_status_t riscv64_start_cpu(cpu_num_t cpu_num, uint32_t hart_id);
extern "C" void riscv64_secondary_entry_asm();

#endif  // !__ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MP_H_
