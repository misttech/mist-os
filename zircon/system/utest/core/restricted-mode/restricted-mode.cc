// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/zx/exception.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/debug.h>
#include <zircon/syscalls/exception.h>
#include <zircon/testonly-syscalls.h>
#include <zircon/threads.h>

#include <mutex>
#include <thread>

#include <runtime/thread.h>
#include <zxtest/zxtest.h>

#include "../needs-next.h"

NEEDS_NEXT_SYSCALL(zx_restricted_bind_state);
NEEDS_NEXT_SYSCALL(zx_restricted_enter);
NEEDS_NEXT_SYSCALL(zx_restricted_kick);
NEEDS_NEXT_SYSCALL(zx_restricted_unbind_state);

extern "C" void vectab();
extern "C" void syscall_bounce();
extern "C" void syscall_bounce_post_syscall();
extern "C" void exception_bounce();
extern "C" void exception_bounce_exception_address();
extern "C" zx_status_t restricted_enter_wrapper(uint32_t options, uintptr_t vector_table,
                                                zx_restricted_reason_t* reason_code);
extern "C" void store_one();
extern "C" void wait_then_syscall();
extern "C" void load_fpu_registers(void* in);
extern "C" void store_fpu_registers(void* out);

// The normal-mode view of restricted-mode state will change slightly depending on
// if the exit to normal-mode was caused by a syscall or an exception.
enum class RegisterMutation {
  kFromSyscall,
  kFromException,
};

#if defined(__x86_64__)

// The number of bytes needed to hold the FPU's state.
// x86 has 8 10-byte registers, followed by 16 16-byte registers.
static const uint16_t kFpuBufferSize = 8 * 10 + 16 * 16;

class ArchRegisterState {
 public:
  static void WritePcToGeneralRegs(zx_thread_state_general_regs_t& regs, uint64_t rip) {
    regs.rip = rip;
  }
  void InitializeRegisters() {
    state_.flags = 0;
    state_.rax = 0x0101010101010101;
    state_.rbx = 0x0202020202020202;
    state_.rcx = 0x0303030303030303;
    state_.rdx = 0x0404040404040404;
    state_.rsi = 0x0505050505050505;
    state_.rdi = 0x0606060606060606;
    state_.rbp = 0x0707070707070707;
    state_.rsp = 0x0808080808080808;
    state_.r8 = 0x0909090909090909;
    state_.r9 = 0x0a0a0a0a0a0a0a0a;
    state_.r10 = 0x0b0b0b0b0b0b0b0b;
    state_.r11 = 0x0c0c0c0c0c0c0c0c;
    state_.r12 = 0x0d0d0d0d0d0d0d0d;
    state_.r13 = 0x0e0e0e0e0e0e0e0e;
    state_.r14 = 0x0f0f0f0f0f0f0f0f;
    state_.r15 = 0x1010101010101010;
    state_.fs_base = reinterpret_cast<uintptr_t>(&fs_val_);
    state_.gs_base = reinterpret_cast<uintptr_t>(&gs_val_);
    fs_val_ = 0;
    gs_val_ = 0;
  }

  void InitializeFromThreadState(const zx_thread_state_general_regs_t& regs) {
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

  void VerifyTwiddledRestrictedState(RegisterMutation mutation) const {
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
    EXPECT_EQ(0x0303030303030304, fs_val_);
    EXPECT_EQ(0x0c0c0c0c0c0c0c0d, gs_val_);
  }

  void VerifyArchSpecificRestrictedState() const {
    // Verify that the flags field does not contain reserved bits. These are rejected by
    // zx_restricted_enter.
    // [intel/vol1]: 3.4.3 EFLAGS Register: Bits 1, 3, 5, 15, and 22 through 31 of this register are
    // reserved. Software should not use or depend on the states of any of these bits.
    constexpr uint64_t kX86ReservedFlagBitss =
        0b11111111'11000000'10000000'00101010 | (0xffffffffull << 32);
    EXPECT_EQ(state_.flags & kX86ReservedFlagBitss, 0);
  }

  uintptr_t pc() { return state_.ip; }
  void set_pc(uintptr_t pc) { state_.ip = pc; }
  zx_restricted_state_t& restricted_state() { return state_; }

 private:
  uint64_t fs_val_ = 0;
  uint64_t gs_val_ = 0;
  zx_restricted_state_t state_{};
};

#elif defined(__aarch64__)

// The number of bytes needed to hold the FPU's state.
// ARM has 32 16-byte floating-point registers.
static const uint16_t kFpuBufferSize = 32 * 16;

class ArchRegisterState {
 public:
  static void WritePcToGeneralRegs(zx_thread_state_general_regs_t& regs, uint64_t pc) {
    regs.pc = pc;
  }

  void InitializeRegisters() {
    // Initialize all standard registers to arbitrary values.
    state_.x[0] = 0x0101010101010101;
    state_.x[1] = 0x0202020202020202;
    state_.x[2] = 0x0303030303030303;
    state_.x[3] = 0x0404040404040404;
    state_.x[4] = 0x0505050505050505;
    state_.x[5] = 0x0606060606060606;
    state_.x[6] = 0x0707070707070707;
    state_.x[7] = 0x0808080808080808;
    state_.x[8] = 0x0909090909090909;
    state_.x[9] = 0x0a0a0a0a0a0a0a0a;
    state_.x[10] = 0x0b0b0b0b0b0b0b0b;
    state_.x[11] = 0x0c0c0c0c0c0c0c0c;
    state_.x[12] = 0x0d0d0d0d0d0d0d0d;
    state_.x[13] = 0x0e0e0e0e0e0e0e0e;
    state_.x[14] = 0x0f0f0f0f0f0f0f0f;
    state_.x[15] = 0x0101010101010101;
    state_.x[16] = 0x0202020202020202;
    state_.x[17] = 0x0303030303030303;
    state_.x[18] = 0x0404040404040404;
    state_.x[19] = 0x0505050505050505;
    state_.x[20] = 0x0606060606060606;
    state_.x[21] = 0x0707070707070707;
    state_.x[22] = 0x0808080808080808;
    state_.x[23] = 0x0909090909090909;
    state_.x[24] = 0x0a0a0a0a0a0a0a0a;
    state_.x[25] = 0x0b0b0b0b0b0b0b0b;
    state_.x[26] = 0x0c0c0c0c0c0c0c0c;
    state_.x[27] = 0x0d0d0d0d0d0d0d0d;
    state_.x[28] = 0x0e0e0e0e0e0e0e0e;
    state_.x[29] = 0x0f0f0f0f0f0f0f0f;
    state_.x[30] = 0x0101010101010101;
    state_.sp = 0x0808080808080808;
    state_.cpsr = 0;

    // Initialize a new thread local storage for the restricted mode routine.
    state_.tpidr_el0 = reinterpret_cast<uintptr_t>(&tls_val_);
  }

  void InitializeFromThreadState(const zx_thread_state_general_regs_t& regs) {
    static_assert(sizeof(regs.r) <= sizeof(state_.x));
    memcpy(state_.x, regs.r, sizeof(regs.r));
    state_.x[30] = regs.lr;
    state_.pc = regs.pc;
    state_.tpidr_el0 = regs.tpidr;
    state_.sp = regs.sp;
    state_.cpsr = static_cast<uint32_t>(regs.cpsr);
  }

  void VerifyTwiddledRestrictedState(RegisterMutation mutation) const {
    // Validate the state of the registers is what was written inside restricted mode.
    //
    // NOTE: Each of the registers was incremented by one before exiting restricted mode.
    // x0 was used as temp space by syscall_bounce, so skip that one.
    EXPECT_EQ(0x0202020202020203, state_.x[1]);
    EXPECT_EQ(0x0303030303030304, state_.x[2]);
    EXPECT_EQ(0x0404040404040405, state_.x[3]);
    EXPECT_EQ(0x0505050505050506, state_.x[4]);
    EXPECT_EQ(0x0606060606060607, state_.x[5]);
    EXPECT_EQ(0x0707070707070708, state_.x[6]);
    EXPECT_EQ(0x0808080808080809, state_.x[7]);
    EXPECT_EQ(0x090909090909090a, state_.x[8]);
    EXPECT_EQ(0x0a0a0a0a0a0a0a0b, state_.x[9]);
    EXPECT_EQ(0x0b0b0b0b0b0b0b0c, state_.x[10]);
    EXPECT_EQ(0x0c0c0c0c0c0c0c0d, state_.x[11]);
    EXPECT_EQ(0x0d0d0d0d0d0d0d0e, state_.x[12]);
    EXPECT_EQ(0x0e0e0e0e0e0e0e0f, state_.x[13]);
    EXPECT_EQ(0x0f0f0f0f0f0f0f10, state_.x[14]);
    EXPECT_EQ(0x0101010101010102, state_.x[15]);
    if (mutation == RegisterMutation::kFromSyscall) {
      EXPECT_EQ(0x40, state_.x[16]);  // syscall_bounce ran syscall 0x40
    } else {
      EXPECT_EQ(0x0202020202020203, state_.x[16]);
    }
    EXPECT_EQ(0x0303030303030304, state_.x[17]);
    EXPECT_EQ(0x0404040404040405, state_.x[18]);
    EXPECT_EQ(0x0505050505050506, state_.x[19]);
    EXPECT_EQ(0x0606060606060607, state_.x[20]);
    EXPECT_EQ(0x0707070707070708, state_.x[21]);
    EXPECT_EQ(0x0808080808080809, state_.x[22]);
    EXPECT_EQ(0x090909090909090a, state_.x[23]);
    EXPECT_EQ(0x0a0a0a0a0a0a0a0b, state_.x[24]);
    EXPECT_EQ(0x0b0b0b0b0b0b0b0c, state_.x[25]);
    EXPECT_EQ(0x0c0c0c0c0c0c0c0d, state_.x[26]);
    EXPECT_EQ(0x0d0d0d0d0d0d0d0e, state_.x[27]);
    EXPECT_EQ(0x0e0e0e0e0e0e0e0f, state_.x[28]);
    EXPECT_EQ(0x0f0f0f0f0f0f0f10, state_.x[29]);
    EXPECT_EQ(0x0101010101010102, state_.x[30]);
    EXPECT_EQ(0x0808080808080809, state_.sp);

    // Check that thread local storage was updated correctly in restricted mode.
    EXPECT_EQ(0x0202020202020203, tls_val_);
  }

  void VerifyArchSpecificRestrictedState() const {}

  uintptr_t pc() { return state_.pc; }
  void set_pc(uintptr_t pc) { state_.pc = pc; }
  zx_restricted_state_t& restricted_state() { return state_; }

 private:
  uint64_t tls_val_ = 0;
  zx_restricted_state_t state_{};
};

#elif defined(__riscv)

// The number of bytes needed to hold the FPU's state.
// RISC-V has 32 8-byte floating-point registers.
static const uint16_t kFpuBufferSize = 32 * 8;

class ArchRegisterState {
 public:
  static void WritePcToGeneralRegs(zx_thread_state_general_regs_t& regs, uint64_t pc) {
    regs.pc = pc;
  }

  void InitializeRegisters() {
    // Initialize all standard registers to arbitrary values.
    state_.ra = 0x0101010101010101;
    state_.sp = 0x0202020202020202;
    state_.gp = 0x0303030303030303;
    state_.tp = reinterpret_cast<uintptr_t>(&tls_val_);
    state_.t0 = 0x0505050505050505;
    state_.t1 = 0x0606060606060606;
    state_.t2 = 0x0707070707070707;
    state_.s0 = 0x0808080808080808;
    state_.s1 = 0x0909090909090909;
    state_.a0 = 0x0a0a0a0a0a0a0a0a;
    state_.a1 = 0x0b0b0b0b0b0b0b0b;
    state_.a2 = 0x0c0c0c0c0c0c0c0c;
    state_.a3 = 0x0d0d0d0d0d0d0d0d;
    state_.a4 = 0x0e0e0e0e0e0e0e0e;
    state_.a5 = 0x0f0f0f0f0f0f0f0f;
    state_.a6 = 0x0101010101010101;
    state_.a7 = 0x0202020202020202;
    state_.s2 = 0x0303030303030303;
    state_.s3 = 0x0404040404040404;
    state_.s4 = 0x0505050505050505;
    state_.s5 = 0x0606060606060606;
    state_.s6 = 0x0707070707070707;
    state_.s7 = 0x0808080808080808;
    state_.s8 = 0x0909090909090909;
    state_.s9 = 0x0a0a0a0a0a0a0a0a;
    state_.s10 = 0x0b0b0b0b0b0b0b0b;
    state_.s11 = 0x0c0c0c0c0c0c0c0c;
    state_.t3 = 0x0d0d0d0d0d0d0d0d;
    state_.t4 = 0x0e0e0e0e0e0e0e0e;
    state_.t5 = 0x0f0f0f0f0f0f0f0f;
    state_.t6 = 0x0101010101010101;
  }

  void InitializeFromThreadState(const zx_thread_state_general_regs_t& regs) {
    static_assert(sizeof(regs) <= sizeof(state_));
    memcpy(&state_, &regs, sizeof(regs));
  }

  void VerifyTwiddledRestrictedState(RegisterMutation mutation) const {
    // Validate the state of the registers is what was written inside restricted mode.
    //
    // NOTE: Each of the registers was incremented by one before exiting restricted mode.
    EXPECT_EQ(0x0101010101010102, state_.ra);
    EXPECT_EQ(0x0202020202020203, state_.sp);
    EXPECT_EQ(0x0303030303030304, state_.gp);
    EXPECT_EQ(reinterpret_cast<uintptr_t>(&tls_val_), state_.tp);
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
    EXPECT_EQ(0x0505050505050506, tls_val_);
  }

  void VerifyArchSpecificRestrictedState() const {}

  uintptr_t pc() { return state_.pc; }
  void set_pc(uintptr_t pc) { state_.pc = pc; }
  zx_restricted_state_t& restricted_state() { return state_; }

 private:
  uint64_t tls_val_ = 0;
  zx_restricted_state_t state_{};
};

#endif

// Verify that restricted_enter handles invalid args.
TEST(RestrictedMode, EnterInvalidArgs) {
  NEEDS_NEXT_SKIP(zx_restricted_enter);

  // Invalid options.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_enter(0xffffffff, 0, 0));

  // Enter restricted mode with invalid args.
  // Vector table must be valid user pointer.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_enter(0, -1, 0));
}

TEST(RestrictedMode, BindState) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  // Bad options.
  zx::vmo v_invalid;
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_bind_state(1, v_invalid.reset_and_get_address()));
  ASSERT_FALSE(v_invalid.is_valid());
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_bind_state(2, v_invalid.reset_and_get_address()));
  ASSERT_FALSE(v_invalid.is_valid());
  ASSERT_EQ(ZX_ERR_INVALID_ARGS,
            zx_restricted_bind_state(0xffffffff, v_invalid.reset_and_get_address()));
  ASSERT_FALSE(v_invalid.is_valid());

  // Happy case.
  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Binding again is fine and replaces any previously bound VMO.
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  ASSERT_TRUE(vmo.is_valid());

  // Map the vmo and verify the state follows.
  zx_vaddr_t ptr = 0;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       zx_system_get_page_size(), &ptr));
  zx_restricted_state_t* state2 = reinterpret_cast<zx_restricted_state_t*>(ptr);

  // Read the state out of the vmo and compare with memory map.
  zx_restricted_state_t state = {};
  ASSERT_OK(vmo.read(&state, 0, sizeof(state)));
  EXPECT_EQ(0, memcmp(state2, &state, sizeof(state)));

  // Fill the state with garbage and make sure it follows.
  memset(state2, 0x99, sizeof(state));
  ASSERT_OK(vmo.read(&state, 0, sizeof(state)));
  EXPECT_EQ(0, memcmp(state2, &state, sizeof(state)));

  // Write garbage via the write syscall and read it back out of the mapping.
  memset(&state, 0x55, sizeof(state));
  ASSERT_OK(vmo.write(&state, 0, sizeof(state)));
  EXPECT_EQ(0, memcmp(state2, &state, sizeof(state)));

  // Teardown the mapping.
  zx::vmar::root_self()->unmap(ptr, zx_system_get_page_size());
}

TEST(RestrictedMode, UnbindState) {
  NEEDS_NEXT_SKIP(zx_restricted_unbind_state);

  // Repeated unbind is OK.
  ASSERT_OK(zx_restricted_unbind_state(0));
  ASSERT_OK(zx_restricted_unbind_state(0));
  ASSERT_OK(zx_restricted_unbind_state(0));

  // Options must be 0.
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_unbind_state(1));
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_unbind_state(1));
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, zx_restricted_unbind_state(0xffffffff));
}

// This is the happy case.
TEST(RestrictedMode, Basic) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  ArchRegisterState state;
  state.InitializeRegisters();

  // Set the PC to the syscall_bounce routine, as the PC is where zx_restricted_enter
  // will jump to.
  state.set_pc(reinterpret_cast<uint64_t>(syscall_bounce));

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));

  // Enter restricted mode with reasonable args, expect a bounce back.
  zx_restricted_reason_t reason_code = 99;
  ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code));
  ASSERT_EQ(ZX_RESTRICTED_REASON_SYSCALL, reason_code);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state.restricted_state(), 0, sizeof(state.restricted_state())));

  state.VerifyArchSpecificRestrictedState();

  // Validate that the instruction pointer is right after the syscall instruction.
  EXPECT_EQ((uintptr_t)&syscall_bounce_post_syscall, state.pc());
  state.VerifyTwiddledRestrictedState(RegisterMutation::kFromSyscall);
}

// Verify that floating point state is saved correctly on context switch.
TEST(RestrictedMode, FloatingPointState) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  const uint32_t kNumRestrictedThreads = 32;
  const uint32_t kNumFloatingPointThreads = 32;
  std::atomic_int num_threads_ready = 0;
  std::atomic_int num_threads_in_rmode = 0;
  std::atomic_int start_restricted_threads = 0;
  std::atomic_int exit_restricted_mode = 0;
  zx_status_t statuses[kNumRestrictedThreads]{};
  std::vector<std::thread> threads;

  for (uint32_t i = 0; i < kNumRestrictedThreads; i++) {
    threads.emplace_back(
        [&exit_restricted_mode, &num_threads_ready, &num_threads_in_rmode,
         &start_restricted_threads, &statuses](int thread_num) {
          // Configure the initial register state.
          zx::vmo vmo;
          zx_status_t status = zx_restricted_bind_state(0, vmo.reset_and_get_address());
          if (status != ZX_OK) {
            statuses[thread_num] = status;
            return;
          }
          auto cleanup = fit::defer([]() { zx_restricted_unbind_state(0); });

          ArchRegisterState state;
          state.InitializeRegisters();
          state.set_pc(reinterpret_cast<uint64_t>(wait_then_syscall));
#if defined(__x86_64__)
          state.restricted_state().rdi = reinterpret_cast<uint64_t>(&num_threads_in_rmode);
          state.restricted_state().rsi = reinterpret_cast<uint64_t>(&exit_restricted_mode);
#elif defined(__aarch64__)  // defined(__x86_64__)
          state.restricted_state().x[0] = reinterpret_cast<uint64_t>(&num_threads_in_rmode);
          state.restricted_state().x[1] = reinterpret_cast<uint64_t>(&exit_restricted_mode);
#elif defined(__riscv)      // defined(__aarch64__)
          state.restricted_state().a0 = reinterpret_cast<uint64_t>(&num_threads_in_rmode);
          state.restricted_state().a1 = reinterpret_cast<uint64_t>(&exit_restricted_mode);
#endif                      // defined(__riscv)

          // Write the state to the state VMO.
          status = vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state()));
          if (status != ZX_OK) {
            statuses[thread_num] = status;
            return;
          }

          // Wait for the main thread to tell us that we can write the FPU state and enter
          // restricted mode. We synchronize this to make sure that all of the restricted threads
          // modify their FPU state at the same time - context switching between the FPU write and
          // the entry into restricted mode can cause the threads to save the FPU state in normal
          // mode, which we do not want.
          num_threads_ready.fetch_add(1);
          while (start_restricted_threads.load() == 0) {
          }

          zx_restricted_reason_t reason_code;
          // Construct the desired FPU state.
          char fpu_buffer[kFpuBufferSize]{};
          memset(fpu_buffer, 0x10101010 + thread_num, kFpuBufferSize);
          // Stack allocate the buffer we're going to use to retrieve the FPU state after exiting
          // restricted mode. We do this before writing the desired FPU state as the process of
          // zeroing out the array modifies the floating-point registers on ARM.
          char got_fpu_buffer[kFpuBufferSize]{};
          load_fpu_registers(fpu_buffer);
          status = restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code);
          if (status != ZX_OK) {
            statuses[thread_num] = status;
            return;
          }
          if (reason_code != ZX_RESTRICTED_REASON_SYSCALL) {
            statuses[thread_num] = ZX_ERR_BAD_STATE;
            return;
          }

          // Validate that the FPU contains the expected contents.
          store_fpu_registers(got_fpu_buffer);
          if (memcmp(fpu_buffer, got_fpu_buffer, kFpuBufferSize) != 0) {
            // Mark this test as a failure.
            statuses[thread_num] = ZX_ERR_BAD_STATE;

            // Print out the diff for easy debugging.
            for (uint16_t i = 0; i < kFpuBufferSize; i++) {
              if (got_fpu_buffer[i] != fpu_buffer[i]) {
                printf("thread %d: byte %u differs; got 0x%x, want 0x%x\n", thread_num, i,
                       got_fpu_buffer[i], fpu_buffer[i]);
              }
            }
          } else {
            statuses[thread_num] = ZX_OK;
          }
        },
        (int)i);
  }

  // Wait for all the threads that will run restricted mode to spawn.
  while (num_threads_ready.load() != kNumRestrictedThreads) {
  }
  // Tell all of the restricted threads to start.
  start_restricted_threads.store(1);

  // Wait for all of the restricted threads to enter restricted mode.
  while (num_threads_in_rmode.load() != kNumRestrictedThreads) {
  }

  // Spawn a bunch of threads that overwrite the contents of the floating point registers.
  // We spawn enough threads to make sure that all CPU's floating point registers are overwritten.
  for (uint32_t i = 0; i < kNumFloatingPointThreads; i++) {
    threads.emplace_back(
        [](int thread_num) {
          char fpu_buffer[kFpuBufferSize]{};
          memset(fpu_buffer, 0x90909090 + thread_num, kFpuBufferSize);
          load_fpu_registers(fpu_buffer);
        },
        (int)i);
  }

  // Signal all of the restricted mode threads to exit, then wait for them to do so.
  exit_restricted_mode.store(1);
  for (auto& thread : threads) {
    thread.join();
  }
  for (uint32_t i = 0; i < kNumRestrictedThreads; i++) {
    ASSERT_OK(statuses[i]);
  }
}

// This is a simple benchmark test that prints some rough performance numbers.
TEST(RestrictedMode, Bench) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  // Run the test 5 times to help filter out noise.
  for (auto i = 0; i < 5; i++) {
    zx::vmo vmo;
    ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
    auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

    // Set the state.
    ArchRegisterState state;
    state.InitializeRegisters();
    state.set_pc(reinterpret_cast<uintptr_t>(syscall_bounce));
    ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));

    // Go through a full restricted syscall entry/exit cycle iter times and show the time.
    {
      zx_restricted_reason_t reason_code;
      auto t = zx::ticks::now();
      auto deadline = t + zx::ticks::per_second();
      int iter = 0;
      while (zx::ticks::now() <= deadline) {
        ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code));
        iter++;
      }
      t = zx::ticks::now() - t;

      printf("restricted call %ld ns per round trip (%ld raw ticks), %d iters\n",
             t / iter * ZX_SEC(1) / zx::ticks::per_second(), t.get(), iter);
    }

    {
      // For way of comparison, time a null syscall.
      auto t = zx::ticks::now();
      auto deadline = t + zx::ticks::per_second();
      int iter = 0;
      while (zx::ticks::now() <= deadline) {
        ASSERT_OK(zx_syscall_test_0());
        iter++;
      }
      t = zx::ticks::now() - t;

      printf("test syscall %ld ns per call (%ld raw ticks), %d iters\n",
             t / iter * ZX_SEC(1) / zx::ticks::per_second(), t.get(), iter);
    }

    // In-thread exception handling
    auto t = zx::ticks::now();
    auto deadline = t + zx::ticks::per_second();
    int iter = 0;
    zx_restricted_reason_t reason_code;
    state.set_pc(reinterpret_cast<uintptr_t>(exception_bounce_exception_address));
    ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));
    while (zx::ticks::now() <= deadline) {
      ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code));
      iter++;
    }
    t = zx::ticks::now() - t;

    printf("in-thread exceptions %ld ns per round trip (%ld raw ticks) %d iters\n",
           t / iter * ZX_SEC(1) / zx::ticks::per_second(), t.get(), iter);
  }
}

// Verify we can receive restricted exceptions using in-thread exception handlers.
TEST(RestrictedMode, InThreadException) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure initial register state.
  ArchRegisterState state;
  state.InitializeRegisters();
  state.set_pc(reinterpret_cast<uint64_t>(exception_bounce));

  // Enter restricted mode. The restricted code will twiddle some registers
  // and generate an exception.
  ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));
  zx_restricted_reason_t reason_code = 99;
  ASSERT_OK(restricted_enter_wrapper(0, reinterpret_cast<uintptr_t>(vectab), &reason_code));

  ASSERT_EQ(ZX_RESTRICTED_REASON_EXCEPTION, reason_code);

  zx_restricted_exception_t exception_state = {};
  ASSERT_OK(vmo.read(&exception_state, 0, sizeof(exception_state)));
  EXPECT_EQ(sizeof(exception_state.exception), exception_state.exception.header.size);
  EXPECT_EQ(ZX_EXCP_UNDEFINED_INSTRUCTION, exception_state.exception.header.type);
  EXPECT_EQ(0u, exception_state.exception.context.synth_code);
  EXPECT_EQ(0u, exception_state.exception.context.synth_data);
#if defined(__x86_64__)
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.x86_64.err_code);
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.x86_64.cr2);
  // 0x6 corresponds to the invalid opcode vector.
  EXPECT_EQ(0x6u, exception_state.exception.context.arch.u.x86_64.vector);
#elif defined(__aarch64__)
  constexpr uint32_t kEsrIlBit = 1ull << 25;
  EXPECT_EQ(kEsrIlBit, exception_state.exception.context.arch.u.arm_64.esr);
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.arm_64.far);
#elif defined(__riscv)
  EXPECT_EQ(0x2, exception_state.exception.context.arch.u.riscv_64.cause);
  EXPECT_EQ(0u, exception_state.exception.context.arch.u.riscv_64.tval);
#endif
}

// Verify that restricted_enter fails on invalid zx_restricted_state_t values.
TEST(RestrictedMode, EnterBadStateStruct) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  ArchRegisterState state;
  state.InitializeRegisters();

  [[maybe_unused]] auto set_state_and_enter = [&]() {
    // Set the state.
    ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));

    // This should fail with bad state.
    ASSERT_EQ(ZX_ERR_BAD_STATE, zx_restricted_enter(0, (uintptr_t)&vectab, 0));
  };

  state.set_pc(-1);  // pc is outside of user space
  set_state_and_enter();

#ifdef __x86_64__
  state.InitializeRegisters();
  state.set_pc((uint64_t)syscall_bounce);
  state.restricted_state().flags = (1UL << 31);  // set an invalid flag
  set_state_and_enter();

  state.InitializeRegisters();
  state.set_pc((uint64_t)syscall_bounce);
  state.restricted_state().fs_base = (1UL << 63);  // invalid fs (non canonical)
  set_state_and_enter();

  state.InitializeRegisters();
  state.set_pc((uint64_t)syscall_bounce);
  state.restricted_state().gs_base = (1UL << 63);  // invalid gs (non canonical)
  set_state_and_enter();
#endif

#ifdef __aarch64__
  state.InitializeRegisters();
  state.set_pc((uint64_t)syscall_bounce);
  state.restricted_state().cpsr = 0x1;  // CPSR contains non-user settable flags.
  set_state_and_enter();
#endif
}

TEST(RestrictedMode, KickBeforeEnter) {
  NEEDS_NEXT_SKIP(zx_restricted_bind_state);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  ArchRegisterState state;
  state.InitializeRegisters();

  // Set the PC to the syscall_bounce routine, as the PC is where zx_restricted_enter
  // will jump to.
  state.set_pc(reinterpret_cast<uint64_t>(syscall_bounce));

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));

  zx::unowned<zx::thread> current_thread(thrd_get_zx_handle(thrd_current()));

  // Issue a kick on ourselves which should apply to the next attempt to enter restricted mode.
  uint32_t options = 0;
  ASSERT_OK(zx_restricted_kick(current_thread->get(), options));

  // Enter restricted mode with reasonable args, expect it to return due to kick and not run any
  // restricted mode code.
  uint64_t reason_code = 99;
  ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code));
  EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_KICK);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state.restricted_state(), 0, sizeof(state.restricted_state())));
  state.VerifyArchSpecificRestrictedState();

  // Validate that the instruction pointer is still pointing at the entry point.
  EXPECT_EQ(reinterpret_cast<uintptr_t>(&syscall_bounce), state.pc());

#if defined(__x86_64__)
  // Validate that the state is unchanged
  EXPECT_EQ(0x0101010101010101, state.restricted_state().rax);
#elif defined(__aarch64__)  // defined(__x86_64__)
  EXPECT_EQ(0x0202020202020202, state.restricted_state().x[1]);
#elif defined(__riscv)      // defined(__aarch64__)
  EXPECT_EQ(0x0b0b0b0b0b0b0b0b, state.restricted_state().a1);
#endif                      // defined(__riscv)

  // Check that the kicked state is cleared
  ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code));
  EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_SYSCALL);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state.restricted_state(), 0, sizeof(state.restricted_state())));
  state.VerifyArchSpecificRestrictedState();

  // Validate that the instruction pointer is right after the syscall instruction.
  EXPECT_EQ(reinterpret_cast<uintptr_t>(&syscall_bounce_post_syscall), state.pc());

  // Validate that the value in first general purpose register is incremented.
  state.VerifyTwiddledRestrictedState(RegisterMutation::kFromSyscall);
}

TEST(RestrictedMode, KickWhileStartingAndExiting) {
  NEEDS_NEXT_SKIP(zx_restricted_kick);

  struct ExceptionChannelRegistered {
    std::condition_variable cv;
    std::mutex m;
    bool registered = false;
  };
  ExceptionChannelRegistered ec;

  struct ChildThreadStarted {
    std::condition_variable cv;
    std::mutex m;
    zx_koid_t koid = 0;
  };
  ChildThreadStarted ct;

  // Register a debugger exception channel so we can intercept thread lifecycle events
  // and issue restricted kicks. This runs on a child thread so that we can process events
  // while the main thread is blocked on the main thread starting and joining.
  std::thread exception_thread([&ec, &ct]() {
    zx::channel exception_channel;
    ASSERT_OK(zx::process::self()->create_exception_channel(ZX_EXCEPTION_CHANNEL_DEBUGGER,
                                                            &exception_channel));
    // Notify the main thread that the exception channel is registered so it knows when to
    // start the test thread.
    {
      std::lock_guard lock(ec.m);
      ec.registered = true;
      ec.cv.notify_one();
    }
    zx_koid_t child_koid;
    // Wait for the child thread to start and tell us its KOID.
    {
      std::unique_lock lock(ct.m);
      ct.cv.wait(lock, [&ct]() { return ct.koid != 0; });
      child_koid = ct.koid;
    }

    // Read exceptions out of the exception channel until we get the first one triggered by
    // the child thread. We do this to avoid a rare race condition in which a
    // ZX_EXCP_THREAD_EXITING triggered by a thread in another test case is delivered to the
    // process exception channel after this test case starts. See https://fxbug.dev/42078955
    // for more info.
    zx_exception_info_t info;
    zx::exception exception;
    zx_info_handle_basic_t handle_info = {};
    zx::thread thread;
    while (handle_info.koid != child_koid) {
      ASSERT_OK(exception_channel.wait_one(ZX_CHANNEL_READABLE, zx::time::infinite(), nullptr));
      ASSERT_OK(exception_channel.read(0, &info, exception.reset_and_get_address(), sizeof(info), 1,
                                       nullptr, nullptr));
      ASSERT_OK(exception.get_thread(&thread));
      size_t actual;
      size_t avail;
      ASSERT_OK(zx_object_get_info(thread.get(), ZX_INFO_HANDLE_BASIC, &handle_info,
                                   sizeof(handle_info), &actual, &avail));
      ASSERT_EQ(actual, 1);
      ASSERT_EQ(avail, 1);
    }

    // Starting child_thread should generate a ZX_EXCP_THREAD_STARTING message on our exception
    // channel.
    ASSERT_EQ(info.type, ZX_EXCP_THREAD_STARTING);

    uint32_t kick_options = 0;
    ASSERT_OK(zx_restricted_kick(thread.get(), kick_options));
    // Release the exception to let the thread start the rest of the way.
    exception.reset();

    // When the thread joins, we expect to receive a ZX_EXCP_THREAD_EXITING message on our exception
    // channel.
    ASSERT_OK(exception_channel.wait_one(ZX_CHANNEL_READABLE, zx::time::infinite(), nullptr));
    ASSERT_OK(exception_channel.read(0, &info, exception.reset_and_get_address(), sizeof(info), 1,
                                     nullptr, nullptr));
    ASSERT_EQ(info.type, ZX_EXCP_THREAD_EXITING);

    ASSERT_OK(exception.get_thread(&thread));
    // Since this thread is now in the DYING state, sending a restricted kick is expected to return
    // ZX_ERR_BAD_STATE.
    EXPECT_EQ(zx_restricted_kick(thread.get(), kick_options), ZX_ERR_BAD_STATE);
  });

  {
    std::unique_lock lock(ec.m);
    ec.cv.wait(lock, [&ec]() { return ec.registered; });
  }

  std::thread child_thread([]() {
    zx::vmo vmo;
    ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
    auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

    ArchRegisterState state;
    state.InitializeRegisters();
    state.set_pc(0u);

    ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));

    // Attempting to enter restricted mode should return immediately with a kick.
    uint64_t reason_code = 99;
    ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)&vectab, &reason_code));
    EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_KICK);
  });

  // Get the KOID of the child thread and communicate it to the exception handler.
  auto child_handle = native_thread_get_zx_handle(child_thread.native_handle());
  size_t actual;
  size_t avail;
  zx_info_handle_basic_t info = {};
  ASSERT_OK(
      zx_object_get_info(child_handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), &actual, &avail));
  ASSERT_EQ(actual, 1);
  ASSERT_EQ(avail, 1);

  {
    std::lock_guard lock(ct.m);
    ct.koid = info.koid;
    ct.cv.notify_one();
  }

  child_thread.join();
  exception_thread.join();
}

TEST(RestrictedMode, KickWhileRunning) {
  NEEDS_NEXT_SKIP(zx_restricted_kick);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  ArchRegisterState state;
  state.InitializeRegisters();
  state.set_pc(reinterpret_cast<uintptr_t>(store_one));

  std::atomic_int flag = 0;

#if defined(__x86_64__)
  state.restricted_state().rax = reinterpret_cast<uint64_t>(&flag);
  state.restricted_state().rbx = 42;
#elif defined(__aarch64__)  // defined(__x86_64__)
  state.restricted_state().x[0] = reinterpret_cast<uint64_t>(&flag);
  state.restricted_state().x[1] = 42;
#elif defined(__riscv)      // defined(__aarch64__)
  state.restricted_state().a0 = reinterpret_cast<uint64_t>(&flag);
  state.restricted_state().a1 = 42;
#endif                      // defined(__riscv)

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));

  zx::unowned<zx::thread> current_thread(thrd_get_zx_handle(thrd_current()));

  // Start up a thread that will enter kick this thread once it detects that 'flag' has been
  // written to, indicating that r-mode code is running.
  std::thread kicker([&flag, &current_thread] {
    // Wait for the first thread to write to 'flag' so we know it's in restricted mode.
    while (flag.load() == 0) {
    }
    // Kick it
    uint32_t options = 0;
    ASSERT_OK(zx_restricted_kick(current_thread->get(), options));
  });

  // Enter restricted mode and expect to tell us that it was kicked out.
  uint64_t reason_code = 99;
  ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code));
  EXPECT_EQ(reason_code, ZX_RESTRICTED_REASON_KICK);

  kicker.join();
  EXPECT_EQ(flag.load(), 1);

  // Read the state out of the thread.
  ASSERT_OK(vmo.read(&state.restricted_state(), 0, sizeof(state.restricted_state())));
  state.VerifyArchSpecificRestrictedState();

  // Expect to see second general purpose register incremented in the observed restricted state.
#if defined(__x86_64__)
  EXPECT_EQ(state.restricted_state().rbx, 43);
#elif defined(__aarch64__)  // defined(__x86_64__)
  EXPECT_EQ(state.restricted_state().x[1], 43);
#elif defined(__riscv)      // defined(__aarch64__)
  EXPECT_EQ(state.restricted_state().a1, 43);
#endif                      // defined(__riscv)
}

TEST(RestrictedMode, KickJustBeforeSyscall) {
  NEEDS_NEXT_SKIP(zx_restricted_kick);

  zx::vmo vmo;
  ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));
  auto cleanup = fit::defer([]() { EXPECT_OK(zx_restricted_unbind_state(0)); });

  // Configure the initial register state.
  ArchRegisterState state;
  state.InitializeRegisters();

  state.set_pc(reinterpret_cast<uint64_t>(wait_then_syscall));

  // Create atomic int 'signal' and 'wait_on'
  std::atomic_int wait_on = 0;
  std::atomic_int signal = 0;

  // Set up state using these variables.
#if defined(__x86_64__)
  state.restricted_state().rdi = reinterpret_cast<uint64_t>(&wait_on);
  state.restricted_state().rsi = reinterpret_cast<uint64_t>(&signal);
#elif defined(__aarch64__)  // defined(__x86_64__)
  state.restricted_state().x[0] = reinterpret_cast<uint64_t>(&wait_on);
  state.restricted_state().x[1] = reinterpret_cast<uint64_t>(&signal);
#elif defined(__riscv)      // defined(__aarch64__)
  state.restricted_state().a0 = reinterpret_cast<uint64_t>(&wait_on);
  state.restricted_state().a1 = reinterpret_cast<uint64_t>(&signal);
#endif                      // defined(__riscv)

  // Write the state to the state VMO.
  ASSERT_OK(vmo.write(&state.restricted_state(), 0, sizeof(state.restricted_state())));

  zx::unowned<zx::thread> current_thread(thrd_get_zx_handle(thrd_current()));

  std::thread kicker([&wait_on, &signal, &current_thread] {
    // Wait until the restricted mode thread is just about to issue a syscall.
    while (wait_on.load() == 0) {
    }
    // Suspend the restricted mode thread before we kick it so we can ensure that it doesn't
    // proceed before the kick is processed.
    zx::suspend_token token;
    ASSERT_OK(current_thread->suspend(&token));
    ASSERT_OK(current_thread->wait_one(ZX_THREAD_SUSPENDED, zx::time::infinite(), nullptr));
    // Issue a kick.
    uint32_t kick_options = 0;
    ASSERT_OK(zx_restricted_kick(current_thread->get(), kick_options));
    // Unsuspend the thread.
    token.reset();
    // Store a signal to release the restricted mode thread so it could issue a syscall
    // if it continues executing. We expect it to come out of thread suspend and process
    // the kick instead of continuing.
    signal.store(1);
  });
  uint64_t reason_code = 99;
  ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &reason_code));
  EXPECT_EQ(ZX_RESTRICTED_REASON_KICK, reason_code);
  kicker.join();
}
