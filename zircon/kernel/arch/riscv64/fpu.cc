// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "arch/riscv64/fpu.h"

#include <stdint.h>

#include <kernel/thread.h>

// Save the current on-cpu fpu state to the thread. Takes as an argument
// the current HW status in the sstatus register.
void riscv64_thread_fpu_save(Thread* thread, Riscv64FpuStatus fpu_status) {
  switch (fpu_status) {
    case Riscv64FpuStatus::DIRTY:
      // The hardware state is dirty, save the old state.
      riscv64_fpu_save(&thread->arch().fpu_state);

      // Record that this thread has modified the state and will need to restore it
      // from now on out on every context switch.
      thread->arch().fpu_dirty = true;
      break;

    // These three states means the thread didn't modify the state, so do not
    // write anything back.
    case Riscv64FpuStatus::INITIAL:
      // The old thread has the initial zeroed state.
    case Riscv64FpuStatus::CLEAN:
      // The old thread has some valid state loaded, but it didn't modify it.
      break;
    case Riscv64FpuStatus::OFF:
      // We currently leave the fpu on all the time, so we should never get this state.
      // If lazy FPU load is implemented, this will be a valid state if the thread has
      // never trapped.
      panic("riscv context switch: FPU was disabled during context switch: sstatus.fs %#lx\n",
            static_cast<unsigned long>(fpu_status));
  }
}

// Restores the fpu state to hardware from the thread passed in. current_state_initial
// should hold whether or not the sstatus.fs bits are currently RISCV64_CSR_SSTATUS_FS_INITIAL,
// as an optimization to avoid re-reading sstatus any more than necessary.
void riscv64_thread_fpu_restore(const Thread* t, Riscv64FpuStatus fpu_status) {
  // Restore the state from the new thread
  if (t->arch().fpu_dirty) {
    riscv64_fpu_restore(&t->arch().fpu_state);

    // Set the fpu hardware state to clean
    riscv64_csr_clear(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_MASK);
    riscv64_csr_set(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_CLEAN);
  } else if (fpu_status != Riscv64FpuStatus::INITIAL) {
    // Zero the state of the fpu unit if it currently is known to have something other
    // than the initial state loaded.
    riscv64_fpu_zero();

    // Set the fpu hardware state to clean
    riscv64_csr_clear(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_MASK);
    riscv64_csr_set(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_INITIAL);
  } else {  // thread does not have dirty state and fpu state == INITIAL
    // The old fpu hardware should have the initial state here and we didn't reset it
    // so we should still be in the initial state.
    DEBUG_ASSERT(fpu_status == Riscv64FpuStatus::INITIAL);
  }
}
