// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_SIGRETURN_UNWINDER_H_
#define SRC_LIB_UNWINDER_SIGRETURN_UNWINDER_H_

#include "src/lib/unwinder/cfi_unwinder.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/registers.h"
#include "src/lib/unwinder/unwinder_base.h"

namespace unwinder {

// Unwind when pc is in a Linux sigreturn function.
class SigReturnUnwinder : public UnwinderBase {
 public:
  // We need |CfiUnwinder::GetMemoryForPc|.
  explicit SigReturnUnwinder(CfiUnwinder* cfi_unwinder) : UnwinderBase(cfi_unwinder) {}

  Error Step(Memory* stack, const Frame& current, Frame& next) override {
    return Step(stack, current.regs, next.regs);
  }

 private:
  Error Step(Memory* stack, const Registers& current, Registers& next);
  Error StepX64(Memory* stack, const Registers& current, Registers& next);
  Error StepArm64(Memory* stack, const Registers& current, Registers& next);
  Error StepRiscv64(Memory* stack, const Registers& current, Registers& next);
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_SIGRETURN_UNWINDER_H_
