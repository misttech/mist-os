// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "arch-register-state.h"

#include <zxtest/zxtest.h>

// The number of bytes needed to hold the FPU's state.
// x86 has 8 10-byte registers, followed by 16 16-byte registers.
const uint16_t kFpuBufferSize = 8 * 10 + 16 * 16;

// Represents the storage used for TLS ABIs.
struct TlsStorage {
  uint64_t fs_val;
  uint64_t gs_val;
};
const size_t kTlsStorageSize = sizeof(TlsStorage);

void ArchRegisterState::InitializeRegisters(TlsStorage* tls_storage) {
  // Configure TLS storage
  set_tls(tls_storage);
  tls()->fs_val = 0;
  tls()->gs_val = 0;

  // Initialize all standard registers to arbitrary values.
  state_ = {
      .rdi = 0x0606060606060606,
      .rsi = 0x0505050505050505,
      .rbp = 0x0707070707070707,
      .rbx = 0x0202020202020202,
      .rdx = 0x0404040404040404,
      .rcx = 0x0303030303030303,
      .rax = 0x0101010101010101,
      .rsp = 0x0808080808080808,
      .r8 = 0x0909090909090909,
      .r9 = 0x0a0a0a0a0a0a0a0a,
      .r10 = 0x0b0b0b0b0b0b0b0b,
      .r11 = 0x0c0c0c0c0c0c0c0c,
      .r12 = 0x0d0d0d0d0d0d0d0d,
      .r13 = 0x0e0e0e0e0e0e0e0e,
      .r14 = 0x0f0f0f0f0f0f0f0f,
      .r15 = 0x1010101010101010,
      .flags = 0,
      .fs_base = reinterpret_cast<uintptr_t>(&tls()->fs_val),
      .gs_base = reinterpret_cast<uintptr_t>(&tls()->gs_val),
  };
}

void ArchRegisterState::InitializeFromThreadState(const zx_thread_state_general_regs_t& regs) {
  state_.flags = regs.rflags;
  state_.rax = regs.rax;
  state_.rbx = regs.rbx;
  state_.rcx = regs.rcx;
  state_.rdx = regs.rdx;
  state_.rsi = regs.rsi;
  state_.rdi = regs.rdi;
  state_.rbp = regs.rbp;
  state_.rsp = regs.rsp;
  state_.r8 = regs.r8;
  state_.r9 = regs.r9;
  state_.r10 = regs.r10;
  state_.r11 = regs.r11;
  state_.r12 = regs.r12;
  state_.r13 = regs.r13;
  state_.r14 = regs.r14;
  state_.r15 = regs.r15;
  state_.fs_base = regs.fs_base;
  state_.gs_base = regs.gs_base;
  state_.ip = regs.rip;
}

void ArchRegisterState::VerifyTwiddledRestrictedState(RegisterMutation mutation) const {
  // Validate the state of the registers is what was written inside restricted mode.
  //
  // NOTE: Each of the registers was incremented by one before exiting restricted mode.
  EXPECT_EQ(0x0101010101010102, state_.rax);
  EXPECT_EQ(0x0202020202020203, state_.rbx);
  if (mutation == RegisterMutation::kFromSyscall) {
    EXPECT_EQ(0, state_.rcx);  // RCX is trashed by the syscall and set to zero
  } else {
    EXPECT_EQ(0x0303030303030304, state_.rcx);
  }
  EXPECT_EQ(0x0404040404040405, state_.rdx);
  EXPECT_EQ(0x0505050505050506, state_.rsi);
  EXPECT_EQ(0x0606060606060607, state_.rdi);
  EXPECT_EQ(0x0707070707070708, state_.rbp);
  EXPECT_EQ(0x0808080808080809, state_.rsp);
  EXPECT_EQ(0x090909090909090a, state_.r8);
  EXPECT_EQ(0x0a0a0a0a0a0a0a0b, state_.r9);
  EXPECT_EQ(0x0b0b0b0b0b0b0b0c, state_.r10);
  if (mutation == RegisterMutation::kFromSyscall) {
    EXPECT_EQ(0, state_.r11);  // r11 is trashed by the syscall and set to zero
  } else {
    EXPECT_EQ(0x0c0c0c0c0c0c0c0d, state_.r11);
  }
  EXPECT_EQ(0x0d0d0d0d0d0d0d0e, state_.r12);
  EXPECT_EQ(0x0e0e0e0e0e0e0e0f, state_.r13);
  EXPECT_EQ(0x0f0f0f0f0f0f0f10, state_.r14);
  EXPECT_EQ(0x1010101010101011, state_.r15);

  // Validate that it was able to write to fs:0 and gs:0 while inside restricted mode the post
  // incremented values of rcx and r11 were written here.
  EXPECT_EQ(0x0303030303030304, tls()->fs_val);
  EXPECT_EQ(0x0c0c0c0c0c0c0c0d, tls()->gs_val);
}

void ArchRegisterState::VerifyArchSpecificRestrictedState() const {
  // Verify that the flags field does not contain reserved bits. These are rejected by
  // zx_restricted_enter.
  // [intel/vol1]: 3.4.3 EFLAGS Register: Bits 1, 3, 5, 15, and 22 through 31 of this register are
  // reserved. Software should not use or depend on the states of any of these bits.
  constexpr uint64_t kX86ReservedFlagBitss =
      0b11111111'11000000'10000000'00101010 | (0xffffffffull << 32);
  EXPECT_EQ(state_.flags & kX86ReservedFlagBitss, 0);
}

uintptr_t ArchRegisterState::pc() const { return state_.ip; }
void ArchRegisterState::set_pc(uintptr_t pc) { state_.ip = pc; }
void ArchRegisterState::set_arg_regs(uint64_t arg0, uint64_t arg1) {
  state_.rdi = arg0;
  state_.rsi = arg1;
}

// This is from RestrictedMode::ArchDump in zircon/kernel.
void ArchRegisterState::PrintState(const zx_restricted_state_t& state) {
  printf(" RIP: %#18" PRIx64 "  FL: %#18" PRIx64 "\n", state.ip, state.flags);
  printf(" RAX: %#18" PRIx64 " RBX: %#18" PRIx64 " RCX: %#18" PRIx64 " RDX: %#18" PRIx64 "\n",
         state.rax, state.rbx, state.rcx, state.rdx);
  printf(" RSI: %#18" PRIx64 " RDI: %#18" PRIx64 " RBP: %#18" PRIx64 " RSP: %#18" PRIx64 "\n",
         state.rsi, state.rdi, state.rbp, state.rsp);
  printf("  R8: %#18" PRIx64 "  R9: %#18" PRIx64 " R10: %#18" PRIx64 " R11: %#18" PRIx64 "\n",
         state.r8, state.r9, state.r10, state.r11);
  printf(" R12: %#18" PRIx64 " R13: %#18" PRIx64 " R14: %#18" PRIx64 " R15: %#18" PRIx64 "\n",
         state.r12, state.r13, state.r14, state.r15);
  printf("fs base %#18" PRIx64 " gs base %#18" PRIx64 "\n", state.fs_base, state.gs_base);
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
