// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/plt_unwinder.h"

#include <cstdint>

#include "src/lib/unwinder/cfi_module.h"

namespace unwinder {

Error PltUnwinder::Step(Memory* stack, const Frame& current, Frame& next) {
  return Step(stack, current.regs, next.regs);
}

Error PltUnwinder::Step(Memory* stack, const Registers& current, Registers& next) {
  switch (current.arch()) {
    case Registers::Arch::kX64:
      return StepX64(stack, current, next);
    case Registers::Arch::kArm64:
      return StepArm64(stack, current, next);
    case Registers::Arch::kRiscv64:
      return StepRiscv64(stack, current, next);
  }
}

Error PltUnwinder::StepX64(Memory* stack, const Registers& current, Registers& next) {
  // The PLT stub looks like
  //
  // 0000000001477870 <printf@plt>:
  //  1477870: ff 25 42 26 1b 00             jmpq    *1779266(%rip)
  //  1477876: 68 06 00 00 00                pushq   $6
  //  147787b: e9 80 ff ff ff                jmp     0x1477800 <.plt>

  uint64_t sp;
  if (auto err = current.GetSP(sp); err.has_err()) {
    return err;
  }
  uint64_t sp_val[2];
  if (auto err = stack->Read(sp, sp_val); err.has_err()) {
    return err;
  }
  uint64_t ra;
  if (cfi_unwinder_->IsValidPC(sp_val[0])) {
    ra = sp_val[0];
    sp += 8;
  } else if (cfi_unwinder_->IsValidPC(sp_val[1])) {
    ra = sp_val[1];
    sp += 16;
  } else {
    return Error("It doesn't look like a PLT trampoline");
  }
  // Simulate a return.
  next = current;
  next.SetPC(ra);
  next.SetSP(sp);
  return Success();
}

Error PltUnwinder::StepArm64(Memory* stack, const Registers& current, Registers& next) {
  // The PLT stub looks like
  //
  // 00000000002d4580 <__stack_chk_fail@plt>:
  //   2d4580: 90000070      adrp    x16, 0x2e0000
  //   2d4584: f9456a11      ldr     x17, [x16, #2768]
  //   2d4588: 912b4210      add     x16, x16, #2768
  //   2d458c: d61f0220      br      x17

  uint64_t lr;
  if (auto err = current.Get(RegisterID::kArm64_lr, lr); err.has_err()) {
    return err;
  }
  uint64_t pc;
  if (auto err = current.GetPC(pc); err.has_err()) {
    return err;
  }

  // Check whether the machine instruction is a PLT entry to avoid false positives.
  // We use "br x17" as a signature, which is d61f0220 represented in little endian.
  CfiModule* cfi_module;
  if (auto err = cfi_unwinder_->GetCfiModuleFor(lr, &cfi_module); err.has_err()) {
    return err;
  }
  uint32_t br_instruction;
  if (auto err = cfi_module->memory()->Read((pc & ~0xf) | 0xc, br_instruction); err.has_err()) {
    return err;
  }
  if (br_instruction != 0xd61f0220) {
    return Error("It doesn't look like a PLT trampoline");
  }

  next = current;
  next.SetPC(lr);
  next.Unset(RegisterID::kArm64_lr);
  next.Unset(RegisterID::kArm64_x16);
  next.Unset(RegisterID::kArm64_x17);
  return Success();
}

Error PltUnwinder::StepRiscv64(Memory* stack, const Registers& current, Registers& next) {
  return Error("not implemented");
}

}  // namespace unwinder
