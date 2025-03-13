// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/fp_unwinder.h"

#include <cinttypes>
#include <cstdint>

#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

namespace {

// The maximum frame size we use when checking whether a frame pointer points to the stack.
// This could be further improved to ask users to provide the stack size.
uint64_t kMaxFrameSize = 8ull * 1024 * 1024;  // 8 MB

}  // namespace

Error FramePointerUnwinder::Step(Memory* stack, const Frame& current, Frame& next) {
  return Step(stack, current.regs, next.regs);
}

Error FramePointerUnwinder::Step(Memory* stack, const Registers& current, Registers& next) {
  RegisterID fp_reg;
  switch (current.arch()) {
    case Registers::Arch::kX64:
      fp_reg = RegisterID::kX64_rbp;
      break;
    case Registers::Arch::kArm64:
      fp_reg = RegisterID::kArm64_x29;
      break;
    case Registers::Arch::kRiscv64:
      fp_reg = RegisterID::kRiscv64_s0;
      break;
  }

  uint64_t fp;
  if (auto err = current.Get(fp_reg, fp); err.has_err()) {
    return err;
  }

  if (current.arch() == Registers::Arch::kRiscv64 && fp >= 16) {
    fp -= 16;
  }

  uint64_t sp;
  if (auto err = current.GetSP(sp); err.has_err()) {
    return err;
  }
  if (fp < sp || fp > sp + kMaxFrameSize) {
    return Error("current FP %#" PRIx64 " doesn't seem to be on the stack", fp);
  }

  uint64_t next_fp;
  if (auto err = stack->ReadAndAdvance(fp, next_fp); err.has_err()) {
    return err;
  }
  // Don't check the range of next_fp, because it may not be used as the frame pointer.

  uint64_t next_pc;
  if (auto err = stack->ReadAndAdvance(fp, next_pc); err.has_err()) {
    return err;
  }
  if (!cfi_unwinder_->IsValidPC(next_pc)) {
    return Error("next PC %#" PRIx64 " is not pointing to any code", next_pc);
  }

  next.SetSP(fp);
  next.SetPC(next_pc);
  next.Set(fp_reg, next_fp);
  return Success();
}

}  // namespace unwinder
