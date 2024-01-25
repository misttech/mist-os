// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/ktrace.h>

#include <arch/ops.h>
#include <arch/spinlock.h>
#include <kernel/spin_tracing.h>
#include <ktl/atomic.h>

namespace {
void on_lock_acquired(arch_spin_lock_t* lock) TA_ASSERT(lock) {
  WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) + 1);
}

inline void arch_spin_lock_core(arch_spin_lock_t* lock, unsigned long val) TA_ACQ(lock) {
  uint64_t temp;

  __asm__ volatile(
      "sevl;"
      "1: wfe;"
      "2: ldaxr   %[temp], [%[lock]];"
      "cbnz    %[temp], 1b;"
      "stxr    %w[temp], %[val], [%[lock]];"
      "cbnz    %w[temp], 2b;"
      : [temp] "=&r"(temp)
      : [lock] "r"(&lock->value), [val] "r"(val)
      : "cc", "memory");

  on_lock_acquired(lock);
}

}  // namespace

void arch_spin_lock_non_instrumented(arch_spin_lock_t* lock) TA_ACQ(lock) {
  unsigned long val = arch_curr_cpu_num() + 1;
  arch_spin_lock_core(lock, val);
}

void arch_spin_lock_trace_instrumented(arch_spin_lock_t* lock,
                                       spin_tracing::EncodedLockId encoded_lock_id) TA_ACQ(lock) {
  unsigned long val = arch_curr_cpu_num() + 1;
  unsigned long expected = 0;

  ktl::atomic_ref<decltype(lock->value)> avalue{lock->value};
  if (avalue.compare_exchange_strong(expected, val, ktl::memory_order_acquire,
                                     ktl::memory_order_relaxed)) {
    on_lock_acquired(lock);
    return;
  }

  spin_tracing::Tracer<true> spin_tracer;
  arch_spin_lock_core(lock, val);
  spin_tracer.Finish(spin_tracing::FinishType::kLockAcquired, encoded_lock_id);
}

bool arch_spin_trylock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  unsigned long val = arch_curr_cpu_num() + 1;
  uint64_t out;
  __asm__ volatile(
      "1:"
      "ldaxr   %[out], [%[lock]];"
      "cbnz    %[out], 2f;"
      "stxr    %w[out], %[val], [%[lock]];"
      // Even though this is a try lock, if the store on acquire fails we try
      // again. This is to prevent spurious failures from misleading the caller
      // into thinking the lock is held by another thread. See
      // https://fxbug.dev/42164720 for details.
      "cbnz    %w[out], 1b;"
      "2:"
      "clrex;"
      : [out] "=&r"(out)
      : [lock] "r"(&lock->value), [val] "r"(val)
      : "cc", "memory");

  if (out == 0) {
    WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) + 1);
  }
  return out;
}

void arch_spin_unlock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) - 1);
  __atomic_store_n(&lock->value, 0UL, __ATOMIC_RELEASE);
}
