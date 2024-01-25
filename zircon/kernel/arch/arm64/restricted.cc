// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch.h>
#include <inttypes.h>
#include <stdlib.h>
#include <trace.h>

#include <arch/arm64/registers.h>
#include <arch/debugger.h>
#include <arch/regs.h>
#include <arch/vm.h>
#include <kernel/restricted_state.h>

#define LOCAL_TRACE 0

void RestrictedState::ArchDump(const zx_restricted_state_t& state) {
  for (size_t i = 0; i < ktl::size(state.x); i++) {
    printf("R%zu: %#18" PRIx64 "\n", i, state.x[i]);
  }
  printf("CPSR: %#18" PRIx32 "\n", state.cpsr);
  printf("PC: %#18" PRIx64 "\n", state.pc);
  printf("SP: %#18" PRIx64 "\n", state.sp);
  printf("TPIDR_EL0: %#18" PRIx64 "\n", state.tpidr_el0);
}

zx_status_t RestrictedState::ArchValidateStatePreRestrictedEntry(
    const zx_restricted_state_t& state) {
  // Validate that PC is within userspace.
  if (unlikely(!is_user_accessible(state.pc))) {
    LTRACEF("fail due to bad PC %#" PRIx64 "\n", state.pc);
    return ZX_ERR_BAD_STATE;
  }
  // Validate that only the NCZV flags of the CPSR are set.
  if (unlikely((state.cpsr & ~kArmUserVisibleFlags) != 0)) {
    LTRACEF("fail due to flags outside of kArmUserVisibleFlags set (%#" PRIx32 ")\n", state.cpsr);
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

void RestrictedState::ArchSaveStatePreRestrictedEntry(ArchSavedNormalState& arch_state) {
  // Save the thread local storage register from normal mode.
  arch_state.tpidr_el0 = __arm_rsr64("tpidr_el0");
}

[[noreturn]] void RestrictedState::ArchEnterRestricted(const zx_restricted_state_t& state) {
  DEBUG_ASSERT(arch_ints_disabled());

  // Copy restricted state to an interrupt frame.
  iframe_t iframe{};
  static_assert(sizeof(iframe.r) <= sizeof(state.x));
  memcpy(iframe.r, state.x, sizeof(iframe.r));
  iframe.lr = state.x[30];
  iframe.usp = state.sp;
  iframe.elr = state.pc;
  iframe.spsr = static_cast<uint64_t>(state.cpsr);

  // Restore TPIDR_EL0 from restricted mode.
  // TODO(https://fxbug.dev/42076040): Eventually the TPIDR register should be
  // inside the iframe.
  __arm_wsr64("tpidr_el0", state.tpidr_el0);

  // Load the new state and enter restricted mode.
  arch_enter_uspace(&iframe);

  __UNREACHABLE;
}

void RestrictedState::ArchRedirectRestrictedExceptionToNormal(
    const ArchSavedNormalState& arch_state, uintptr_t vector_table, uintptr_t context) {
  zx_thread_state_general_regs_t regs = {};
  regs.pc = vector_table;
  regs.r[0] = context;
  regs.r[1] = ZX_RESTRICTED_REASON_EXCEPTION;
  regs.tpidr = arch_state.tpidr_el0;
  [[maybe_unused]] zx_status_t status = arch_set_general_regs(Thread::Current().Get(), &regs);
  // This will only fail if register state has not been saved, but this will always
  // have happened by this stage of exception handling.
  DEBUG_ASSERT(status == ZX_OK);
}

void RestrictedState::ArchSaveRestrictedExceptionState(zx_restricted_state_t& state) {
  zx_thread_state_general_regs_t regs = {};
  [[maybe_unused]] zx_status_t status = arch_get_general_regs(Thread::Current().Get(), &regs);
  // This will only fail if register state has not been saved, but this will always
  // have happened by this stage of exception handling.
  DEBUG_ASSERT(status == ZX_OK);

  // Save the registers from restricted mode.
  static_assert(sizeof(regs.r) <= sizeof(state.x));
  memcpy(state.x, regs.r, sizeof(regs.r));
  state.x[30] = regs.lr;
  state.sp = regs.sp;
  state.pc = regs.pc;
  state.cpsr = static_cast<uint32_t>(regs.cpsr);

  // Save the thread local storage location in restricted mode.
  state.tpidr_el0 = __arm_rsr64("tpidr_el0");
}

void RestrictedState::ArchSaveRestrictedSyscallState(zx_restricted_state_t& state,
                                                     const syscall_regs_t& regs) {
  DEBUG_ASSERT(arch_ints_disabled());

  // Save the registers from restricted mode.
  static_assert(sizeof(regs.r) <= sizeof(state.x));
  memcpy(state.x, regs.r, sizeof(regs.r));
  state.x[30] = regs.lr;
  state.sp = regs.usp;
  state.pc = regs.elr;

  // Save the thread local storage location in restricted mode.
  state.tpidr_el0 = __arm_rsr64("tpidr_el0");

  // Save only the non-reserved portions of the SPSR.
  state.cpsr = static_cast<uint32_t>(regs.spsr);
}

void RestrictedState::ArchSaveRestrictedIframeState(zx_restricted_state_t& state,
                                                    const iframe_t& frame) {
  // On arm64, iframe_t and syscalls_regs_t are the same type.
  static_assert(ktl::is_same_v<syscall_regs_t, iframe_t>);
  RestrictedState::ArchSaveRestrictedSyscallState(state, frame);
}

[[noreturn]] void RestrictedState::ArchEnterFull(const ArchSavedNormalState& arch_state,
                                                 uintptr_t vector_table, uintptr_t context,
                                                 uint64_t code) {
  DEBUG_ASSERT(arch_ints_disabled());

  // Restore TPIDR_EL0 from saved normal state.
  // TODO(https://fxbug.dev/42076040): Eventually the TPIDR register should be
  // inside the iframe.
  __arm_wsr64("tpidr_el0", arch_state.tpidr_el0);

  // Set up a mostly empty iframe and return back to normal mode.
  iframe_t iframe{};

  // Pass through the context and return code as arguments.
  iframe.r[0] = context;
  iframe.r[1] = code;

  // Set the ELR such that we return to the vector_table after entering normal
  // mode.
  iframe.elr = vector_table;

  // Load the new state and exit.
  arch_enter_uspace(&iframe);

  __UNREACHABLE;
}
