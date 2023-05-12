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
#include <arch/vm.h>
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

void arch_setup_uspace_iframe(iframe_t* iframe, uintptr_t pc, uintptr_t sp, uintptr_t arg1,
                              uintptr_t arg2) {
  iframe->regs.pc = pc;
  iframe->regs.sp = sp;
  iframe->regs.a0 = arg1;
  iframe->regs.a1 = arg2;

  // Saved interrupt enable (so that interrupts are enabled when returning to user space).
  // Current interrupt enable state set to disabled, which will matter when the
  // arch_uspace_entry loads sstatus temporarily before switching to user space.
  // Set user space bitness to 64bit.
  // Set the fpu to the initial state, with the implicit assumption that the context switch
  // routine would have defaulted the FPU state a the time this thread enters user space.
  // All other bits set to zero, default options.
  iframe->status =
      RISCV64_CSR_SSTATUS_PIE | RISCV64_CSR_SSTATUS_UXL_64BIT | RISCV64_CSR_SSTATUS_FS_INITIAL;
}

// Switch to user mode, set the user stack pointer to user_stack_top, save the
// top of the kernel stack pointer.
void arch_enter_uspace(iframe_t* iframe) {
  Thread* ct = Thread::Current::Get();

  LTRACEF("pc %#" PRIx64 " sp %#" PRIx64 " a0 %#" PRIx64 " a1 %#" PRIx64 "\n", iframe->regs.pc,
          ct->stack().top(), iframe->regs.a0, iframe->regs.a1);

  ASSERT(arch_is_valid_user_pc(iframe->regs.pc));

  arch_disable_ints();

#if __has_feature(shadow_call_stack)
  riscv64_uspace_entry(iframe, ct->stack().top(), ct->stack().shadow_call_base());
#else
  riscv64_uspace_entry(iframe, ct->stack().top());
#endif
  __UNREACHABLE;
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
    riscv64_thread_fpu_save(oldthread, current_fpu_status);
  }
  // Always restore the new threads fpu state even if it is probably going to be restored
  // by a higher layer later with a call to arch_restore_user_state. Though it may be extra
  // work in this case, it avoids potential issues with state getting out of sync if the
  // kernel panicked or the higher layer forgot to restore.
  riscv64_thread_fpu_restore(newthread, current_fpu_status);

  // Set the percpu in_restricted_mode field.
  const bool in_restricted =
      newthread->restricted_state() != nullptr && newthread->restricted_state()->in_restricted();
  arch_set_restricted_flag(in_restricted);

  // Regular integer context switch.
  riscv64_context_switch(&oldthread->arch().sp, newthread->arch().sp);
}

void arch_dump_thread(const Thread* t) {
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
  riscv64_thread_fpu_save(thread, riscv64_fpu_status());
  // Not saving debug state because there isn't any.
}

void arch_restore_user_state(Thread* thread) {
  riscv64_thread_fpu_restore(thread, riscv64_fpu_status());
}

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
