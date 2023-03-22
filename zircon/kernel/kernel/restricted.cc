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
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm.h>

#define LOCAL_TRACE 0

// Kernel implementation of restricted mode. Most of these routines are more or less directly
// called from a corresponding syscall. The rest are up called from architecturally specific
// hardware traps, such as an exception or syscall when the cpu is in restricted mode.

// Dispatched directly from arch-specific syscall handler. Called after saving state
// on the stack, but before trying to dispatch as a zircon syscall.
extern "C" [[noreturn]] void syscall_from_restricted(const syscall_regs_t* regs) {
  LTRACEF("regs %p\n", regs);

  DEBUG_ASSERT(regs);
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
  RestrictedState::ArchSaveRestrictedSyscallState(*state, *regs);

  LTRACEF("returning to normal mode at vector %#lx, context %#lx\n", rs->vector_ptr(),
          rs->context());

  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  vmm_set_active_aspace(up->normal_aspace_ptr());

  // bounce into normal mode
  RestrictedState::ArchEnterFull(rs->arch_normal_state(), rs->vector_ptr(), rs->context(), 0);

  __UNREACHABLE;
}

// entry points

zx_status_t RestrictedEnter(uint32_t options, uintptr_t vector_table_ptr, uintptr_t context) {
  LTRACEF("options %#x vector %#" PRIx64 " context %#" PRIx64 "\n", options, vector_table_ptr,
          context);

  // no options defined for the moment
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  // validate the vector table pointer
  if (!is_user_accessible(vector_table_ptr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // has the state buffer been previously bound to this thread?
  RestrictedState* rs = Thread::Current::restricted_state();
  if (rs == nullptr) {
    return ZX_ERR_BAD_STATE;
  }
  DEBUG_ASSERT(!rs->in_restricted());
  const zx_restricted_state_t* state_buffer = rs->state_ptr();
  if (!state_buffer) {
    return ZX_ERR_BAD_STATE;
  }

  // copy out of the buffer here to local object so the state can be
  // pre-validated and used without a possibility of user space manipulating it.
  zx_restricted_state_t state;
  state = *state_buffer;

  if constexpr (LOCAL_TRACE) {
    RestrictedState::ArchDump(state);
  }

  // validate the user state is valid (PC is in user space, etc)
  zx_status_t status = RestrictedState::ArchValidateStatePreRestrictedEntry(state);
  if (unlikely(status != ZX_OK)) {
    return status;
  }

  // from now on out we're committed, disable interrupts so we can do this
  // without being interrupted as we save/restore state
  arch_disable_ints();

  // no more errors or interrupts, so we can switch the active aspace
  // without worrying about ending up in a situation where the thread is
  // set to normal with the restricted aspace active.
  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  VmAspace* restricted_aspace = up->restricted_aspace();
  // This check can be removed once the restricted mode tests can and do run with a restricted
  // aspace.
  if (restricted_aspace) {
    vmm_set_active_aspace(restricted_aspace);
  }

  // save the vector table pointer for return from restricted mode
  rs->set_vector_ptr(vector_table_ptr);

  // save the context ptr for return from restricted mode
  rs->set_context(context);

  // set our state to restricted enabled at the thread and arch level
  rs->set_in_restricted(true);
  arch_set_restricted_flag(true);

  // get a chance to save some state before we enter normal mode
  RestrictedState::ArchSaveStatePreRestrictedEntry(rs->arch_normal_state());

  // enter restricted mode
  RestrictedState::ArchEnterRestricted(state);

  // does not return
  __UNREACHABLE;
}
