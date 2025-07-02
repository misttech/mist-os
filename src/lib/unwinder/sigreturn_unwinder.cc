// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/sigreturn_unwinder.h"

#include <array>
#include <cstdint>

namespace unwinder {

Error SigReturnUnwinder::Step(Memory* stack, const Registers& current, Registers& next) {
  switch (current.arch()) {
    case Registers::Arch::kX64:
      return StepX64(stack, current, next);
    case Registers::Arch::kArm32:
    case Registers::Arch::kArm64:
      return StepArm64(stack, current, next);
    case Registers::Arch::kRiscv64:
      return StepRiscv64(stack, current, next);
  }
}

Error SigReturnUnwinder::StepX64(Memory* stack, const Registers& current, Registers& next) {
  return Error("not implemented");
}

Error SigReturnUnwinder::StepArm32(Memory* stack, const Registers& current, Registers& next) {
  return Error("not implemented");
}

Error SigReturnUnwinder::StepArm64(Memory* stack, const Registers& current, Registers& next) {
  // The sigreturn function looks like:
  //
  // 00000000000001d0 <__kernel_rt_sigreturn>:
  //      1d0: d2801168      mov     x8, #0x8b
  //      1d4: d4000001      svc     #0

  uint64_t pc;
  if (Error error = current.GetPC(pc); error.has_err()) {
    return error;
  }

  // Get the memory containing the pc to check for the sigreturn sequence in the
  // vDSO. Avoid attempting to load the module info using GetCfiModuleInfoForPc,
  // which will likely fail because the vDSO does not usually have .eh_frame or
  // .debug_frame sections.
  Memory* memory;
  if (Error error = cfi_unwinder_->GetMemoryForPc(pc, &memory); error.has_err()) {
    return error;
  }

  // Check for the sigreturn instruction sequence.
  uint64_t instructions;
  if (Error error = memory->Read(pc, instructions); error.has_err()) {
    return error;
  }
  if (instructions != 0xd4000001d2801168ULL) {
    return Error("It doesn't look like a sigreturn function");
  }

  // The sp points to an rt_sigframe:
  //
  // 128 byte siginfo struct
  // ucontext struct:
  //     8 byte long: uc_flags
  //     8 byte pointer: uc_link
  //    24 byte stack_t
  //   128 byte signal set
  //     8 byte padding to 16 byte align sigcontext
  //       sigcontext

  const uint64_t sigcontext_offset = 128 + 8 + 8 + 24 + 128 + 8;
  const uint64_t regs_offset = sigcontext_offset + 8;

  next = current;

  uint64_t sp;
  if (Error error = current.GetSP(sp); error.has_err()) {
    return error;
  }

  // GPRs, sp, and pc are stored in sigcontext in the same order as aadwarf64
  // names registers.
  for (size_t i = 0; i < static_cast<size_t>(RegisterID::kArm64_last); i++) {
    uint64_t gpr;
    if (Error error = stack->Read(sp + regs_offset + (i * sizeof(gpr)), gpr); error.has_err()) {
      return error;
    }
    next.Set(static_cast<RegisterID>(i), gpr);
  }

  return Success();
}

Error SigReturnUnwinder::StepRiscv64(Memory* stack, const Registers& current, Registers& next) {
  return Error("not implemented");
}

}  // namespace unwinder
