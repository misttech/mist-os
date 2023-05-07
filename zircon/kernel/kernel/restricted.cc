// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "kernel/restricted.h"

#include <arch.h>
#include <inttypes.h>
#include <lib/zx/result.h>
#include <stdlib.h>
#include <trace.h>
#include <zircon/syscalls-next.h>

#include <arch/regs.h>
#include <fbl/alloc_checker.h>
#include <kernel/restricted_state.h>
#include <kernel/thread.h>
#include <object/exception_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm.h>

#define LOCAL_TRACE 0

// `state` must be the first member of these structures.
static_assert(offsetof(zx_restricted_syscall_t, state) == 0);
static_assert(offsetof(zx_restricted_exception_t, state) == 0);

// Kernel implementation of restricted mode. Most of these routines are more or less directly
// called from a corresponding syscall. The rest are up called from architecturally specific
// hardware traps, such as an exception or syscall when the cpu is in restricted mode.

template <typename T>
[[noreturn]] void RestrictedLeave(const T* restricted_state_source, zx_restricted_reason_t reason) {
  LTRACEF("regs %p\n", restricted_state_source);

  DEBUG_ASSERT(restricted_state_source);

  // Interrupts are disabled and must remain disabled throughout this function.  Because syscall
  // instructions issued from Restricted Mode do not follow the typical path through the kernel, we
  // must keep interrupts disabled to ensure that we don't "drop" a thread signal.  If a thread
  // signal is currently pending, then the IPI that was sent when it was asserted will fire once we
  // return to user mode.
  DEBUG_ASSERT(arch_ints_disabled());

  RestrictedState* rs = Thread::Current::restricted_state();
  DEBUG_ASSERT(rs != nullptr);

  // assert that some things make sense
  DEBUG_ASSERT(rs->in_restricted() == true);
  DEBUG_ASSERT(is_user_accessible(rs->vector_ptr()));

  // set on the thread and the arch level that we're exiting restricted mode
  rs->set_in_restricted(false);
  arch_set_restricted_flag(false);

  // save the restricted state
  zx_restricted_state_t* state = rs->state_ptr();
  DEBUG_ASSERT(state);
  static_assert(internal::is_copy_allowed<std::remove_reference_t<decltype((*state))>>::value);
  if constexpr (ktl::is_same_v<T, syscall_regs_t>) {
    RestrictedState::ArchSaveRestrictedSyscallState(*state, *restricted_state_source);
  } else {
    static_assert(ktl::is_same_v<T, iframe_t>);
    RestrictedState::ArchSaveRestrictedIframeState(*state, *restricted_state_source);
  }

  LTRACEF("returning to normal mode at vector %#lx, context %#lx\n", rs->vector_ptr(),
          rs->context());

  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  vmm_set_active_aspace(up->normal_aspace_ptr());

  // bounce into normal mode
  RestrictedState::ArchEnterFull(rs->arch_normal_state(), rs->vector_ptr(), rs->context(), reason);

  __UNREACHABLE;
}

// Dispatched directly from arch-specific syscall handler. Called after saving state
// on the stack, but before trying to dispatch as a zircon syscall.
extern "C" [[noreturn]] void syscall_from_restricted(const syscall_regs_t* regs) {
  RestrictedLeaveSyscall(regs, ZX_RESTRICTED_REASON_SYSCALL);
  __UNREACHABLE;
}

// entry points

zx_status_t RestrictedEnter(uint32_t options, uintptr_t vector_table_ptr, uintptr_t context) {
  LTRACEF("options %#x vector %#" PRIx64 " context %#" PRIx64 "\n", options, vector_table_ptr,
          context);

  // validate the vector table pointer
  if (!is_user_accessible(vector_table_ptr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // has the state buffer been previously bound to this thread?
  Thread* const current_thread = Thread::Current::Get();
  RestrictedState* rs = current_thread->restricted_state();
  if (rs == nullptr) {
    return ZX_ERR_BAD_STATE;
  }
  DEBUG_ASSERT(!rs->in_restricted());

  rs->set_in_thread_exceptions_enabled(!(options & ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL));

  const zx_restricted_state_t* state_buffer = rs->state_ptr();
  if (!state_buffer) {
    return ZX_ERR_BAD_STATE;
  }

  // Copy the state out of the state buffer and into an automatic variable.  User mode may have a
  // mapping of the state buffer so it's critical that we make a copy and then validate the copy
  // before using the copy to avoid a ToCToU vulnerability.  Use an atomic_signal_fence as a
  // compiler barrier to ensure the copy actually happens.
  zx_restricted_state_t state;
  static_assert(internal::is_copy_allowed<decltype(state)>::value);
  state = *state_buffer;
  ktl::atomic_signal_fence(ktl::memory_order_seq_cst);

  if constexpr (LOCAL_TRACE) {
    RestrictedState::ArchDump(state);
  }

  // validate the user state is valid (PC is in user space, etc)
  zx_status_t status = RestrictedState::ArchValidateStatePreRestrictedEntry(state);
  if (unlikely(status != ZX_OK)) {
    return status;
  }

  // We need to check for pending signals "manually" because in the non-error-return path this
  // syscall doesn't actually return and unwind the callstack.  Instead, it effectively jumps into
  // (restricted) user mode, bypassing the code in the syscall wrapper / kernel entrypoint that
  // calls Thread::Current::ProcessPendingSignals.
  //
  // As usual, we must disable interrupts prior to checking for pending signals.
  arch_disable_ints();
  if (current_thread->IsSignaled()) {
    // TODO(https://fxbug.dev/126791): Ideally, we'd call Thread::Current::ProcessPendingSignals
    // here, however, we don't have a pointer to the user register state that was pushed on the
    // stack when we entered the kernel.  Instead, we return the special RETRY error so that unwind
    // the call stack and call Thread::Current::ProcessPendingSignals in the usual spot.  We'll
    // return to (normal) user mode and the vDSO will automatically retry the syscall, all
    // transparent to the vDSO caller.  It's OK to enable interrupts here because we're committed to
    // a path that will call Thread::Current::ProcessPendingSignals before returning to user mode.
    arch_enable_ints();
    return ZX_ERR_INTERNAL_INTR_RETRY;
  }

  // Save any arch-specific normal mode state before we enter restricted mode.
  RestrictedState::ArchSaveStatePreRestrictedEntry(rs->arch_normal_state());

  // Save the context and vector table pointer for return from restricted mode.
  rs->set_vector_ptr(vector_table_ptr);
  rs->set_context(context);

  // Now that the normal mode state has been saved, we can decide if we're going to continue on with
  // the mode switch or simply vector-return to normal mode because of a pending kick.
  if (Thread::Current::CheckForRestrictedKick()) {
    RestrictedState::ArchEnterFull(rs->arch_normal_state(), vector_table_ptr, context,
                                   ZX_RESTRICTED_REASON_KICK);
    __UNREACHABLE;
  }

  // We're committed.  Once we've started the switch to restricted mode, we need to finish and enter
  // user mode to ensure the thread's active aspace and "in restricted mode" flags are consistent
  // with the thread being in restricted mode.  No error returns from here on out.  Interrupts must
  // remain disabled.
  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  VmAspace* restricted_aspace = up->restricted_aspace();
  // This check can be removed once the restricted mode tests can and do run with a restricted
  // aspace.
  if (restricted_aspace) {
    vmm_set_active_aspace(restricted_aspace);
  }
  rs->set_in_restricted(true);
  arch_set_restricted_flag(true);
  RestrictedState::ArchEnterRestricted(state);

  __UNREACHABLE;
}

void RedirectRestrictedExceptionToNormalMode(RestrictedState* rs) {
  DEBUG_ASSERT(rs->in_restricted());
  zx_restricted_state_t* state = rs->state_ptr();
  DEBUG_ASSERT(state);

  // Save the exception register state into the restricted state.
  RestrictedState::ArchSaveRestrictedExceptionState(*state);

  // Redirect the exception so that we return to normal mode for handling.
  //
  // This will update the exception context so that when we return back to usermode
  // we will be in normal mode instead of restricted mode.
  RestrictedState::ArchRedirectRestrictedExceptionToNormal(rs->arch_normal_state(),
                                                           rs->vector_ptr(), rs->context());

  // Update state to be in normal mode.
  rs->set_in_restricted(false);
  arch_set_restricted_flag(false);
  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  vmm_set_active_aspace(up->normal_aspace_ptr());
}

[[noreturn]] void RestrictedLeaveIframe(const iframe_t* iframe, zx_restricted_reason_t reason) {
  RestrictedLeave(iframe, reason);

  __UNREACHABLE;
}

[[noreturn]] void RestrictedLeaveSyscall(const syscall_regs_t* regs,
                                         zx_restricted_reason_t reason) {
  RestrictedLeave(regs, reason);

  __UNREACHABLE;
}
