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
  frame->*riscv64_context_switch_frame::kShadowCallStackPointer = t->stack().shadow_call_base();
#endif

  // set the thread pointer that will be restored on the first context switch
  frame->tp = reinterpret_cast<uintptr_t>(&t->arch().thread_pointer_location);
}

void arch_thread_construct_first(Thread* t) { arch_set_current_thread(t); }

// TODO-rvbringup: move these fpu helper routines into a separate fpu.cc file

enum class Riscv64FpuStatus : uint64_t { OFF, INITIAL, CLEAN, DIRTY };

static Riscv64FpuStatus riscv64_fpu_status() {
  uint64_t status = riscv64_csr_read(RISCV64_CSR_SSTATUS);

  // Convert the 2 bit field in sstatus.fs to an enum. The enum is
  // numbered the same as the raw values so the transformation should
  // just be a bitfield extraction.
  switch (status & RISCV64_CSR_SSTATUS_FS_MASK) {
    default:
    case RISCV64_CSR_SSTATUS_FS_OFF:
      return Riscv64FpuStatus::OFF;
    case RISCV64_CSR_SSTATUS_FS_INITIAL:
      return Riscv64FpuStatus::INITIAL;
    case RISCV64_CSR_SSTATUS_FS_CLEAN:
      return Riscv64FpuStatus::CLEAN;
    case RISCV64_CSR_SSTATUS_FS_DIRTY:
      return Riscv64FpuStatus::DIRTY;
  }
}

// Save the current on-cpu fpu state to the thread. Takes as an argument
// the current HW status in the sstatus register.
static void thread_fpu_save(Thread* thread, Riscv64FpuStatus fpu_status) {
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
            fpu_status);
  }
}

// Restores the fpu state to hardware from the thread passed in. current_state_initial
// should hold whether or not the sstatus.fs bits are currently RISCV64_CSR_SSTATUS_FS_INITIAL,
// as an optimization to avoid re-reading sstatus any more than necessary.
static void thread_fpu_restore(Thread* t, Riscv64FpuStatus fpu_status) {
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

void arch_context_switch(Thread* oldthread, Thread* newthread) {
  DEBUG_ASSERT(arch_ints_disabled());

  LTRACEF("old %p (%s), new %p (%s)\n", oldthread, oldthread->name(), newthread, newthread->name());

  // FPU context switch
  // Based on a combination of the current hardware state and whether or not the threads have
  // a dirty flag set, conditionally save and/or restore hardware state.
  if constexpr (LOCAL_TRACE) {
    uint64_t status = riscv64_csr_read(RISCV64_CSR_SSTATUS);
    uint64_t fpu_status = status & RISCV64_CSR_SSTATUS_FS_MASK;
    LTRACEF("fpu: sstatus.fp %#lx, sd %u, old.dirty %u, new.dirty %u\n",
            fpu_status >> RISCV64_CSR_SSTATUS_FS_SHIFT, !!(status & RISCV64_CSR_SSTATUS_SD),
            oldthread->arch().fpu_dirty, newthread->arch().fpu_dirty);
  }

  Riscv64FpuStatus current_fpu_status = riscv64_fpu_status();
  if (likely(!oldthread->IsUserStateSavedLocked())) {
    // Save the fpu state for the old (current) thread, returns whether or not
    // the fpu hardware is currently in the initial state.
    DEBUG_ASSERT(oldthread == Thread::Current().Get());
    thread_fpu_save(oldthread, current_fpu_status);
  }
  // Always restore the new threads fpu state even if it is probably going to be restored
  // by a higher layer later with a call to arch_restore_user_state. Though it may be extra
  // work in this case, it avoids potential issues with state getting out of sync if the
  // kernel panicked or the higher layer forgot to restore.
  thread_fpu_restore(newthread, current_fpu_status);

  // Regular integer context switch.
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

void arch_save_user_state(Thread* thread) {
  thread_fpu_save(thread, riscv64_fpu_status());
  // Not saving debug state because there isn't any.
}

void arch_restore_user_state(Thread* thread) { thread_fpu_restore(thread, riscv64_fpu_status()); }

void arch_set_suspended_general_regs(struct Thread* thread, GeneralRegsSource source,
                                     void* iframe) {
  DEBUG_ASSERT(thread->arch().suspended_general_regs == nullptr);
  DEBUG_ASSERT(iframe != nullptr);
  DEBUG_ASSERT_MSG(source == GeneralRegsSource::Iframe, "invalid source %u\n",
                   static_cast<uint32_t>(source));
  thread->arch().suspended_general_regs = static_cast<iframe_t*>(iframe);
}

void arch_reset_suspended_general_regs(struct Thread* thread) {
  thread->arch().suspended_general_regs = nullptr;
}
