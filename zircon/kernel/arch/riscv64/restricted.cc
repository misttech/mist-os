// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include <arch.h>
#include <inttypes.h>
#include <trace.h>

#include <arch/interrupt.h>
#include <arch/vm.h>
#include <kernel/restricted_state.h>
#include <ktl/iterator.h>

#define LOCAL_TRACE 0

void RestrictedState::ArchDump(const zx_restricted_state_t& state) {
  printf("PC: %#18" PRIx64 "\n", state.pc);
  printf("RA: %#18" PRIx64 "\n", state.ra);
  printf("SP: %#18" PRIx64 "\n", state.sp);
  printf("GP: %#18" PRIx64 "\n", state.gp);
  printf("TP: %#18" PRIx64 "\n", state.tp);
  printf("T0: %#18" PRIx64 "\n", state.t0);
  printf("T1: %#18" PRIx64 "\n", state.t1);
  printf("T2: %#18" PRIx64 "\n", state.t2);
  printf("S0: %#18" PRIx64 "\n", state.s0);
  printf("S1: %#18" PRIx64 "\n", state.s1);
  printf("A0: %#18" PRIx64 "\n", state.a0);
  printf("A1: %#18" PRIx64 "\n", state.a1);
  printf("A2: %#18" PRIx64 "\n", state.a2);
  printf("A3: %#18" PRIx64 "\n", state.a3);
  printf("A4: %#18" PRIx64 "\n", state.a4);
  printf("A5: %#18" PRIx64 "\n", state.a5);
  printf("A6: %#18" PRIx64 "\n", state.a6);
  printf("A7: %#18" PRIx64 "\n", state.a7);
  printf("S2: %#18" PRIx64 "\n", state.s2);
  printf("S3: %#18" PRIx64 "\n", state.s3);
  printf("S4: %#18" PRIx64 "\n", state.s4);
  printf("S5: %#18" PRIx64 "\n", state.s5);
  printf("S6: %#18" PRIx64 "\n", state.s6);
  printf("S7: %#18" PRIx64 "\n", state.s7);
  printf("S8: %#18" PRIx64 "\n", state.s8);
  printf("S9: %#18" PRIx64 "\n", state.s9);
  printf("S10: %#18" PRIx64 "\n", state.s10);
  printf("S11: %#18" PRIx64 "\n", state.s11);
  printf("T3: %#18" PRIx64 "\n", state.t3);
  printf("T4: %#18" PRIx64 "\n", state.t4);
  printf("T5: %#18" PRIx64 "\n", state.t5);
  printf("T6: %#18" PRIx64 "\n", state.t6);
}

zx_status_t RestrictedState::ArchValidateStatePreRestrictedEntry(
    const zx_restricted_state_t& state) {
  // Validate that PC is within userspace.
  if (unlikely(!is_user_accessible(state.pc))) {
    LTRACEF("fail due to bad PC %#" PRIx64 "\n", state.pc);
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

void RestrictedState::ArchSaveStatePreRestrictedEntry(ArchSavedNormalState& arch_state) {}

[[noreturn]] void RestrictedState::ArchEnterRestricted(const zx_restricted_state_t& state) {
  DEBUG_ASSERT(arch_ints_disabled());

  // Create an iframe for restricted mode and set the status to a reasonable
  // initial value.
  iframe_t iframe{
      .status =
          RISCV64_CSR_SSTATUS_PIE | RISCV64_CSR_SSTATUS_UXL_64BIT | RISCV64_CSR_SSTATUS_FS_INITIAL,
      .regs = state,
  };

  // Enter userspace.
  arch_enter_uspace(&iframe);

  __UNREACHABLE;
}

void RestrictedState::ArchSaveRestrictedSyscallState(zx_restricted_state_t& state,
                                                     const syscall_regs_t& regs) {
  DEBUG_ASSERT(arch_ints_disabled());
  state = regs.regs;
}

void RestrictedState::ArchSaveRestrictedIframeState(zx_restricted_state_t& state,
                                                    const iframe_t& frame) {
  // On riscv64, iframe_t and syscalls_regs_t are the same type.
  ArchSaveRestrictedSyscallState(state, frame);
}

[[noreturn]] void RestrictedState::ArchEnterFull(const ArchSavedNormalState& arch_state,
                                                 uintptr_t vector_table, uintptr_t context,
                                                 uint64_t code) {
  DEBUG_ASSERT(arch_ints_disabled());

  // Set up a mostly empty iframe to return back to normal mode.
  iframe_t iframe{};

  // Pass the context and return code as arguments.
  iframe.regs.a0 = context;
  iframe.regs.a1 = code;

  // Set the program counter so we jump to vector_table in normal mode.
  iframe.regs.pc = vector_table;

  // Set status to a valid initial value, but assume the FPU is dirty.
  iframe.status =
      RISCV64_CSR_SSTATUS_PIE | RISCV64_CSR_SSTATUS_UXL_64BIT | RISCV64_CSR_SSTATUS_FS_DIRTY;

  // Enter normal mode.
  arch_enter_uspace(&iframe);

  __UNREACHABLE;
}

void RestrictedState::ArchRedirectRestrictedExceptionToNormal(
    const ArchSavedNormalState& arch_state, uintptr_t vector_table, uintptr_t context) {
  // TODO(https://fxbug.dev/121512): Implement in-thread exceptions on risc-v.
}

void RestrictedState::ArchSaveRestrictedExceptionState(zx_restricted_state_t& state) {
  // TODO(https://fxbug.dev/121512): Implement in-thread exceptions on risc-v.
}
