// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/ops.h>
#include <arch/spinlock.h>
#include <ktl/atomic.h>

// We need to disable thread safety analysis in this file, since we're
// implementing the locks themselves.  Without this, the header-level
// annotations cause Clang to detect violations.

// Simple spinning lock, using LR/SC CAS instructions. Stores the current cpu
// number + 1 for debugging purposes.

void arch_spin_lock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  const uint32_t new_value = arch_curr_cpu_num() + 1;
  for (;;) {
    uint32_t expected = 0;
    if (lock->value.compare_exchange_weak(expected, new_value, ktl::memory_order_acquire,
                                          ktl::memory_order_relaxed)) {
      break;
    }
  }

  WRITE_PERCPU_FIELD32(num_spinlocks, READ_PERCPU_FIELD32(num_spinlocks) + 1);
}

bool arch_spin_trylock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  const uint32_t new_value = arch_curr_cpu_num() + 1;
  uint32_t expected = 0;
  if (lock->value.compare_exchange_strong(expected, new_value, ktl::memory_order_acquire,
                                          ktl::memory_order_relaxed)) {
    // success
    WRITE_PERCPU_FIELD32(num_spinlocks, READ_PERCPU_FIELD32(num_spinlocks) + 1);
  }
  return expected;  // actual old value
}

void arch_spin_unlock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  WRITE_PERCPU_FIELD32(num_spinlocks, READ_PERCPU_FIELD32(num_spinlocks) - 1);
  lock->value.store(0, ktl::memory_order_release);
}
