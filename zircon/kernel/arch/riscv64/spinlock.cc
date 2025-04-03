// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/ops.h>
#include <arch/spinlock.h>
#include <kernel/spin_tracing.h>
#include <ktl/atomic.h>

// Simple spinning lock, using LR/SC CAS instructions. Stores the current cpu
// number + 1 for debugging purposes.

namespace {
void on_lock_acquired(arch_spin_lock_t* lock) TA_ASSERT(lock) {
  WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) + 1);
}
}  // namespace

void arch_spin_lock_non_instrumented(arch_spin_lock_t* lock) {
  const cpu_num_t new_value = arch_curr_cpu_num() + 1;
  for (;;) {
    cpu_num_t expected = 0;
    if (lock->value.compare_exchange_weak(expected, new_value, ktl::memory_order_acquire,
                                          ktl::memory_order_relaxed)) {
      break;
    }
    arch::Yield();
  }

  on_lock_acquired(lock);
}

void arch_spin_lock_trace_instrumented(arch_spin_lock_t* lock,
                                       spin_tracing::EncodedLockId encoded_lock_id) {
  const cpu_num_t new_value = arch_curr_cpu_num() + 1;
  cpu_num_t expected = 0;

  if (lock->value.compare_exchange_strong(expected, new_value, ktl::memory_order_acquire,
                                          ktl::memory_order_relaxed)) {
    on_lock_acquired(lock);
    return;
  }

  spin_tracing::Tracer<true> spin_tracer;
  for (;;) {
    expected = 0;
    if (lock->value.compare_exchange_weak(expected, new_value, ktl::memory_order_acquire,
                                          ktl::memory_order_relaxed)) {
      break;
    }
    arch::Yield();
  }
  spin_tracer.Finish(spin_tracing::FinishType::kLockAcquired, encoded_lock_id);

  on_lock_acquired(lock);
}

bool arch_spin_trylock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  const cpu_num_t new_value = arch_curr_cpu_num() + 1;
  cpu_num_t expected = 0;
  if (lock->value.compare_exchange_strong(expected, new_value, ktl::memory_order_acquire,
                                          ktl::memory_order_relaxed)) {
    // success
    WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) + 1);
  }
  return expected;  // actual old value
}

void arch_spin_unlock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) - 1);
  lock->value.store(0, ktl::memory_order_release);
}
