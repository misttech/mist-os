// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "arch-register-state.h"

#include <zxtest/zxtest.h>

struct TlsStorage {
  uint64_t tpidr;
};
const size_t kTlsStorageSize = sizeof(TlsStorage);

// The number of bytes needed to hold the FPU's state.
// ARM has 32 16-byte floating-point registers.
const uint16_t kFpuBufferSize = 32 * 16;

void ArchRegisterState::InitializeRegisters(TlsStorage* tls_storage) {
  // Initialize a new thread local storage for the restricted mode routine.
  set_tls(tls_storage);
  tls()->tpidr = 0;

  // Initialize all standard registers to arbitrary values.
  state_ = {
      .x =
          {
              0x0101010101010101,  // x[0]
              0x0202020202020202,  // x[1]
              0x0303030303030303,  // x[2]
              0x0404040404040404,  // x[3]
              0x0505050505050505,  // x[4]
              0x0606060606060606,  // x[5]
              0x0707070707070707,  // x[6]
              0x0808080808080808,  // x[7]
              0x0909090909090909,  // x[8]
              0x0a0a0a0a0a0a0a0a,  // x[9]
              0x0b0b0b0b0b0b0b0b,  // x[10]
              0x0c0c0c0c0c0c0c0c,  // x[11]
              0x0d0d0d0d0d0d0d0d,  // x[12]
              0x0e0e0e0e0e0e0e0e,  // x[13]
              0x0f0f0f0f0f0f0f0f,  // x[14]
              0x0101010101010101,  // x[15]
              0x0202020202020202,  // x[16]
              0x0303030303030303,  // x[17]
              0x0404040404040404,  // x[18]
              0x0505050505050505,  // x[19]
              0x0606060606060606,  // x[20]
              0x0707070707070707,  // x[21]
              0x0808080808080808,  // x[22]
              0x0909090909090909,  // x[23]
              0x0a0a0a0a0a0a0a0a,  // x[24]
              0x0b0b0b0b0b0b0b0b,  // x[25]
              0x0c0c0c0c0c0c0c0c,  // x[26]
              0x0d0d0d0d0d0d0d0d,  // x[27]
              0x0e0e0e0e0e0e0e0e,  // x[28]
              0x0f0f0f0f0f0f0f0f,  // x[29]
              0x0101010101010101,  // x[30]
          },
      // Keep the SP 16-byte aligned, as required by the spec.
      .sp = 0x0808080808080810,
      .tpidr_el0 = reinterpret_cast<uintptr_t>(&tls()->tpidr),
      .cpsr = 0,
  };
}

void ArchRegisterState::InitializeFromThreadState(const zx_thread_state_general_regs_t& regs) {
  static_assert(sizeof(regs.r) <= sizeof(state_.x));
  memcpy(state_.x, regs.r, sizeof(regs.r));
  state_.x[30] = regs.lr;
  state_.pc = regs.pc;
  state_.tpidr_el0 = regs.tpidr;
  state_.sp = regs.sp;
  state_.cpsr = static_cast<uint32_t>(regs.cpsr);
}

void ArchRegisterState::VerifyTwiddledRestrictedState(RegisterMutation mutation) const {
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
  EXPECT_EQ(0x0808080808080820, state_.sp);

  // Check that thread local storage was updated correctly in restricted mode.
  EXPECT_EQ(0x0202020202020203, tls()->tpidr);
}

void ArchRegisterState::VerifyArchSpecificRestrictedState() const {}

uintptr_t ArchRegisterState::pc() const { return state_.pc; }
void ArchRegisterState::set_pc(uintptr_t pc) { state_.pc = pc; }
void ArchRegisterState::set_arg_regs(uint64_t arg0, uint64_t arg1) {
  state_.x[0] = arg0;
  state_.x[1] = arg1;
}

// This is from RestrictedMode::ArchDump in zircon/kernel.
void ArchRegisterState::PrintState(const zx_restricted_state_t& state) {
  // Helper for log analysis for unexpected exceptions.
  for (size_t i = 0; i < std::size(state.x); i++) {
    printf("R%zu: %#18" PRIx64 "\n", i, state.x[i]);
  }
  printf("CPSR: %#18" PRIx32 "\n", state.cpsr);
  printf("PC: %#18" PRIx64 "\n", state.pc);
  printf("SP: %#18" PRIx64 "\n", state.sp);
  printf("TPIDR_EL0: %#18" PRIx64 "\n", state.tpidr_el0);
}

void ArchRegisterState::PrintExceptionState(const zx_restricted_exception_t& exc) {
  printf("type: 0x%x\n", exc.exception.header.type);
  printf("synth_code: 0x%x\n", exc.exception.context.synth_code);
  printf("synth_data: 0x%x\n", exc.exception.context.synth_data);
  PrintState(exc.state);
  printf("ESR: 0x%x\n", exc.exception.context.arch.u.arm_64.esr);
  printf("FAR: 0x%lx\n", exc.exception.context.arch.u.arm_64.far);
}

// This derived class handles the differences between aarch64 and aarch32 in the
// same tests.
class Arch32RegisterState : public ArchRegisterState {
 public:
  void InitializeRegisters(TlsStorage* tls_storage) override {
    ArchRegisterState::InitializeRegisters(tls_storage);
    state_.cpsr = 0x10;
  }

  void InitializeFromThreadState(const zx_thread_state_general_regs_t& regs) override {
    static_assert(sizeof(regs.r) <= sizeof(state_.x));
    ASSERT_EQ(regs.cpsr & 0x10, 0x10);
    memcpy(state_.x, regs.r, sizeof(regs.r));
    state_.x[14] = regs.lr;
    state_.x[15] = regs.pc;
    state_.pc = regs.pc;
    state_.tpidr_el0 = regs.tpidr;
    state_.sp = regs.sp;
    state_.cpsr = static_cast<uint32_t>(regs.cpsr);
  }

  void VerifyTwiddledRestrictedState(RegisterMutation mutation) const override {
    // Validate the state of the registers is what was written inside restricted mode.
    //
    // NOTE: Each of the registers was incremented by one before exiting restricted mode.
    // x0 was used as temp space by syscall_bounce, so skip that one.
    EXPECT_EQ(0x0000000002020203, state_.x[1]);
    EXPECT_EQ(0x0000000003030304, state_.x[2]);
    EXPECT_EQ(0x0000000004040405, state_.x[3]);
    EXPECT_EQ(0x0000000005050506, state_.x[4]);
    EXPECT_EQ(0x0000000006060607, state_.x[5]);
    EXPECT_EQ(0x0000000007070708, state_.x[6]);
    EXPECT_EQ(0x000000000909090a, state_.x[8]);
    EXPECT_EQ(0x000000000a0a0a0b, state_.x[9]);
    EXPECT_EQ(0x000000000b0b0b0c, state_.x[10]);
    EXPECT_EQ(0x000000000c0c0c0d, state_.x[11]);
    EXPECT_EQ(0x000000000d0d0d0e, state_.x[12]);
    if (mutation == RegisterMutation::kFromSyscall) {
      EXPECT_EQ(0x40, state_.x[7]);  // syscall_bounce ran syscall 0x40
    } else {
      EXPECT_EQ(0x0000000008080809, state_.x[7]);
    }
    EXPECT_EQ(0x000000000e0e0e1e, state_.x[13]);  // aarch32 sp
    EXPECT_EQ(0x000000000f0f0f0f, state_.x[14]);  // aarch32 lr
    // Registers above 15 will not be saved/updated in restricted state.
    EXPECT_EQ(0x0202020202020202, state_.x[16]);  // unchanged

    // Check that thread local storage was updated correctly in restricted mode.
    EXPECT_EQ(0x0000000008080809, tls()->tpidr);
  }

  void set_pc(uintptr_t pc) override {
    state_.pc = pc;
    state_.x[15] = pc;
  }
};

// Map the types to machines.
std::unique_ptr<ArchRegisterState> ArchRegisterStateFactory::Create(elfldltl::ElfMachine machine) {
  if (machine == elfldltl::ElfMachine::kNative) {
    return std::make_unique<ArchRegisterState>();
  }
  ZX_ASSERT(machine == elfldltl::ElfMachine::kArm);
  return std::make_unique<Arch32RegisterState>();
}
