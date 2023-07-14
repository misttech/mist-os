// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_SPINLOCK_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_SPINLOCK_H_

#include <lib/zircon-internal/thread_annotations.h>

#include <arch/riscv64/mp.h>
#include <kernel/cpu.h>
#include <ktl/atomic.h>

#define ARCH_SPIN_LOCK_INITIAL_VALUE \
  (arch_spin_lock_t) { 0 }

struct TA_CAP("mutex") arch_spin_lock_t {
  ktl::atomic<cpu_num_t> value;
};

// Note: trylock operations are not permitted to fail spuriously, even on
// architectures with weak memory ordering.  If a trylock operation fails, it
// must be because the lock was actually observed to be held by another thread
// during the attempt.
void arch_spin_lock(arch_spin_lock_t* lock) TA_ACQ(lock);
bool arch_spin_trylock(arch_spin_lock_t* lock) TA_TRY_ACQ(false, lock);
void arch_spin_unlock(arch_spin_lock_t* lock) TA_REL(lock);

inline cpu_num_t arch_spin_lock_holder_cpu(const arch_spin_lock_t* lock) {
  return lock->value.load(ktl::memory_order_relaxed) - 1;
}

inline bool arch_spin_lock_held(const arch_spin_lock_t* lock) {
  return arch_spin_lock_holder_cpu(lock) == arch_curr_cpu_num();
}

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_SPINLOCK_H_
