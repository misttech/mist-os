// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <debug.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <trace.h>

#include <arch/riscv64.h>
#include <arch/riscv64/mp.h>
#include <kernel/thread.h>

#define LOCAL_TRACE 0

// assert that the context switch frame is a multiple of 16 to maintain
// stack alignment requirements per ABI
static_assert(sizeof(riscv64_context_switch_frame) % 16 == 0);

void arch_thread_initialize(Thread* t, vaddr_t entry_point) {
  // zero out the entire arch state, including fpu state, which defaults to all zero
  t->arch() = {};

  // create a default stack frame on the stack
  vaddr_t stack_top = t->stack().top();

  // make sure the top of the stack is 16 byte aligned for ABI compliance
  DEBUG_ASSERT(IS_ALIGNED(stack_top, 16));

  struct riscv64_context_switch_frame* frame = (struct riscv64_context_switch_frame*)(stack_top);
  frame--;

  // fill in the entry point
  frame->ra = entry_point;

  // set the stack pointer
  t->arch().sp = (vaddr_t)frame;

#if __has_feature(shadow_call_stack)
  // the shadow call stack grows up
  frame->s2 = t->stack().shadow_call_base();
#endif

  // set the thread pointer that will be restored on the first context switch
  frame->tp = reinterpret_cast<uintptr_t>(&t->arch().thread_pointer_location);
}

void arch_thread_construct_first(Thread* t) { arch_set_current_thread(t); }

void arch_context_switch(Thread* oldthread, Thread* newthread) {
  DEBUG_ASSERT(arch_ints_disabled());

  LTRACEF("old %p (%s), new %p (%s)\n", oldthread, oldthread->name(), newthread, newthread->name());

  // FPU context switch
  // Based on a combination of the current hardware state and whether or not the threads have
  // a dirty flag set, conditionally save and/or restore hardware state.
  uint64_t status = riscv64_csr_read(RISCV64_CSR_SSTATUS);
  uint64_t fpu_status = status & RISCV64_CSR_SSTATUS_FS_MASK;
  LTRACEF("fpu: sstatus.fp %#lx, sd %u, old.dirty %u, new.dirty %u\n",
          fpu_status >> RISCV64_CSR_SSTATUS_FS_SHIFT, !!(status & RISCV64_CSR_SSTATUS_SD),
          oldthread->arch().fpu_dirty, newthread->arch().fpu_dirty);

  bool current_state_initial = false;
  switch (fpu_status) {
    case RISCV64_CSR_SSTATUS_FS_DIRTY:
      // The hardware state is dirty, save the old state.
      riscv64_fpu_save(&oldthread->arch().fpu_state);

      // Record that this thread has modified the state and will need to restore it
      // from now on out on every context switch.
      oldthread->arch().fpu_dirty = true;
      break;

    // These three states means the thread didn't modify the state, so do not
    // write anything back.
    case RISCV64_CSR_SSTATUS_FS_INITIAL:
      // The old thread has the initial zeroed state. Remember this for use below.
      current_state_initial = true;
      break;
    case RISCV64_CSR_SSTATUS_FS_CLEAN:
      // The old thread has some valid state loaded, but it didn't modify it.
      break;
    case RISCV64_CSR_SSTATUS_FS_OFF:
      // We currently leave the fpu on all the time, so we should never get this state.
      // If lazy FPU load is implemented, this will be a valid state if the thread has
      // never trapped.
      panic("riscv context switch: FPU was disabled during context switch: sstatus %#lx\n", status);
  }

  // Restore the state from the new thread
  if (newthread->arch().fpu_dirty) {
    riscv64_fpu_restore(&newthread->arch().fpu_state);

    // Set the fpu hardware state to clean
    riscv64_csr_clear(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_MASK);
    riscv64_csr_set(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_CLEAN);
  } else if (!current_state_initial) {
    // Zero the state of the fpu unit if it currently is known to have something other
    // than the initial state loaded.
    riscv64_fpu_zero();

    // Set the fpu hardware state to clean
    riscv64_csr_clear(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_MASK);
    riscv64_csr_set(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_FS_INITIAL);
  } else {
    // The old fpu hardware should have the initial state here and we didn't reset it
    // so we should still be in the initial state.
    DEBUG_ASSERT((riscv64_csr_read(RISCV64_CSR_SSTATUS) & RISCV64_CSR_SSTATUS_FS_MASK) ==
                 RISCV64_CSR_SSTATUS_FS_INITIAL);
  }

  // regular integer context switch
  riscv64_context_switch(&oldthread->arch().sp, newthread->arch().sp);
}

void arch_dump_thread(Thread* t) {
  if (t->state() != THREAD_RUNNING) {
    dprintf(INFO, "\tarch: ");
    dprintf(INFO, "sp 0x%lx\n", t->arch().sp);
  }
}

vaddr_t arch_thread_get_blocked_fp(Thread* t) {
  if (!WITH_FRAME_POINTERS) {
    return 0;
  }

  const struct riscv64_context_switch_frame* frame;
  frame = reinterpret_cast<const riscv64_context_switch_frame*>(t->arch().sp);
  DEBUG_ASSERT(frame);

  return frame->s0;
}

__NO_SAFESTACK void arch_save_user_state(Thread* thread) {}

__NO_SAFESTACK void arch_restore_user_state(Thread* thread) {}

void arch_set_suspended_general_regs(struct Thread* thread, GeneralRegsSource source,
                                     void* iframe) {}

void arch_reset_suspended_general_regs(struct Thread* thread) {}
