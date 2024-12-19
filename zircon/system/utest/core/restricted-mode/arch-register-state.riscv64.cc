// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "arch-register-state.h"

#include <zxtest/zxtest.h>

// The number of bytes needed to hold the FPU's state.
// RISC-V has 32 8-byte floating-point registers.
const uint16_t kFpuBufferSize = 32 * 8;

// Defines the TlsStorage type RISCV needs.
struct TlsStorage {
  uint64_t tp;
};
const size_t kTlsStorageSize = sizeof(TlsStorage);

void ArchRegisterState::InitializeRegisters(TlsStorage* tls_storage) {
  // Configure TLS storage.
  set_tls(tls_storage);
  tls()->tp = 0;

  // Initialize all standard registers to arbitrary values.
  state_ = {
      .ra = 0x0101010101010101,
      .sp = 0x0202020202020202,
      .gp = 0x0303030303030303,
      .tp = reinterpret_cast<uintptr_t>(&tls()->tp),
      .t0 = 0x0505050505050505,
      .t1 = 0x0606060606060606,
      .t2 = 0x0707070707070707,
      .s0 = 0x0808080808080808,
      .s1 = 0x0909090909090909,
      .a0 = 0x0a0a0a0a0a0a0a0a,
      .a1 = 0x0b0b0b0b0b0b0b0b,
      .a2 = 0x0c0c0c0c0c0c0c0c,
      .a3 = 0x0d0d0d0d0d0d0d0d,
      .a4 = 0x0e0e0e0e0e0e0e0e,
      .a5 = 0x0f0f0f0f0f0f0f0f,
      .a6 = 0x0101010101010101,
      .a7 = 0x0202020202020202,
      .s2 = 0x0303030303030303,
      .s3 = 0x0404040404040404,
      .s4 = 0x0505050505050505,
      .s5 = 0x0606060606060606,
      .s6 = 0x0707070707070707,
      .s7 = 0x0808080808080808,
      .s8 = 0x0909090909090909,
      .s9 = 0x0a0a0a0a0a0a0a0a,
      .s10 = 0x0b0b0b0b0b0b0b0b,
      .s11 = 0x0c0c0c0c0c0c0c0c,
      .t3 = 0x0d0d0d0d0d0d0d0d,
      .t4 = 0x0e0e0e0e0e0e0e0e,
      .t5 = 0x0f0f0f0f0f0f0f0f,
      .t6 = 0x0101010101010101,
  };
}

void ArchRegisterState::InitializeFromThreadState(const zx_thread_state_general_regs_t& regs) {
  static_assert(sizeof(regs) <= sizeof(state_));
  memcpy(&state_, &regs, sizeof(regs));
}

void ArchRegisterState::VerifyTwiddledRestrictedState(RegisterMutation mutation) const {
  // Validate the state of the registers is what was written inside restricted mode.
  //
  // NOTE: Each of the registers was incremented by one before exiting restricted mode.
  EXPECT_EQ(0x0101010101010102, state_.ra);
  EXPECT_EQ(0x0202020202020203, state_.sp);
  EXPECT_EQ(0x0303030303030304, state_.gp);
  EXPECT_EQ(reinterpret_cast<uintptr_t>(&tls()->tp), state_.tp);
  if (mutation == RegisterMutation::kFromSyscall) {
    EXPECT_EQ(0x40, state_.t0);
  } else {
    EXPECT_EQ(0x0505050505050506, state_.t0);
  }
  EXPECT_EQ(0x0606060606060607, state_.t1);
  EXPECT_EQ(0x0707070707070708, state_.t2);
  EXPECT_EQ(0x0808080808080809, state_.s0);
  EXPECT_EQ(0x090909090909090a, state_.s1);
  EXPECT_EQ(0x0a0a0a0a0a0a0a0b, state_.a0);
  EXPECT_EQ(0x0b0b0b0b0b0b0b0c, state_.a1);
  EXPECT_EQ(0x0c0c0c0c0c0c0c0d, state_.a2);
  EXPECT_EQ(0x0d0d0d0d0d0d0d0e, state_.a3);
  EXPECT_EQ(0x0e0e0e0e0e0e0e0f, state_.a4);
  EXPECT_EQ(0x0f0f0f0f0f0f0f10, state_.a5);
  EXPECT_EQ(0x0101010101010102, state_.a6);
  EXPECT_EQ(0x0202020202020203, state_.a7);
  EXPECT_EQ(0x0303030303030304, state_.s2);
  EXPECT_EQ(0x0404040404040405, state_.s3);
  EXPECT_EQ(0x0505050505050506, state_.s4);
  EXPECT_EQ(0x0606060606060607, state_.s5);
  EXPECT_EQ(0x0707070707070708, state_.s6);
  EXPECT_EQ(0x0808080808080809, state_.s7);
  EXPECT_EQ(0x090909090909090a, state_.s8);
  EXPECT_EQ(0x0a0a0a0a0a0a0a0b, state_.s9);
  EXPECT_EQ(0x0b0b0b0b0b0b0b0c, state_.s10);
  EXPECT_EQ(0x0c0c0c0c0c0c0c0d, state_.s11);
  EXPECT_EQ(0x0d0d0d0d0d0d0d0e, state_.t3);
  EXPECT_EQ(0x0e0e0e0e0e0e0e0f, state_.t4);
  EXPECT_EQ(0x0f0f0f0f0f0f0f10, state_.t5);
  EXPECT_EQ(0x0101010101010102, state_.t6);

  // Check that thread local storage was updated correctly in restricted mode.
  EXPECT_EQ(0x0505050505050506, tls()->tp);
}

void ArchRegisterState::VerifyArchSpecificRestrictedState() const {}

uintptr_t ArchRegisterState::pc() const { return state_.pc; }
void ArchRegisterState::set_pc(uintptr_t pc) { state_.pc = pc; }
void ArchRegisterState::set_arg_regs(uint64_t arg0, uint64_t arg1) {
  state_.a0 = arg0;
  state_.a1 = arg1;
}

// This is from RestrictedMode::ArchDump in zircon/kernel.
void ArchRegisterState::PrintState(const zx_restricted_state_t& state) {
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

void ArchRegisterState::PrintExceptionState(const zx_restricted_exception_t& exc) {
  printf("type: 0x%x\n", exc.exception.header.type);
  printf("synth_code: 0x%x\n", exc.exception.context.synth_code);
  printf("synth_data: 0x%x\n", exc.exception.context.synth_data);
  PrintState(exc.state);
}

// Map the types to machines.
std::unique_ptr<ArchRegisterState> ArchRegisterStateFactory::Create(elfldltl::ElfMachine machine) {
  assert(machine == elfldltl::ElfMachine::kNative);
  return std::make_unique<ArchRegisterState>();
}
