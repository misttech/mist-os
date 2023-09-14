// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/scs_unwinder.h"

#include "src/lib/unwinder/registers.h"

namespace unwinder {

Error ShadowCallStackUnwinder::Step(Memory* scs, const Registers& current, Registers& next) {
  RegisterID scs_reg;
  switch (current.arch()) {
    case Registers::Arch::kX64:
      return Error("Shadow call stack is not supported on x64");
    case Registers::Arch::kArm64:
      scs_reg = RegisterID::kArm64_x18;
      break;
    case Registers::Arch::kRiscv64:
      scs_reg = RegisterID::kRiscv64_gp;
      break;
  }
  uint64_t scsp;  // shadow call stack pointer
  if (auto err = current.Get(scs_reg, scsp); err.has_err()) {
    return err;
  }
  if (!scsp) {
    return Error("shadow call stack is not available");
  }

  // The shadow call stack is pushed/popped via (e.g., on arm64)
  //
  //    str     x30, [x18], #8    ; post-indexed
  //    ...
  //    ldr     x30, [x18, #-8]!  ; pre-indexed
  //
  // So x18 points to the next available slots. The same applies to riscv64.
  uint64_t ra;
  if (auto err = scs->Read(scsp - 8, ra); err.has_err()) {
    return err;
  }

  // A zero ra indicates the beginning of the shadow call stack.
  if (!ra) {
    return Success();
  }
  if (!cfi_unwinder_->IsValidPC(ra)) {
    return Error("Invalid shadow call stack");
  }

  next.SetPC(ra);
  next.Set(scs_reg, scsp - 8);
  return Success();
}

}  // namespace unwinder
