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
  uint64_t pc = 0;
  auto e = current.regs.GetPC(pc);

  CfiModuleInfo* info;
  if (auto e = cfi_unwinder_->GetCfiModuleInfoForPc(pc, &info); e.has_err()) {
    return e;
  }

  return Step(stack, current.regs, next.regs, info->module.size);
}

Error FramePointerUnwinder::Step(Memory* stack, const Registers& current, Registers& next,
                                 Module::AddressSize pointer_size) {
  RegisterID fp_reg;
  switch (current.arch()) {
    case Registers::Arch::kX64:
      fp_reg = RegisterID::kX64_rbp;
      break;
    case Registers::Arch::kArm32:
      return Error("Not implemented yet.");
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
  uint64_t next_pc;
  if (auto err = ReadNextFpAndSp(stack, fp, next_fp, next_pc, pointer_size); err.has_err()) {
    return err;
  }

  next.SetSP(fp);
  next.SetPC(next_pc);
  next.Set(fp_reg, next_fp);
  return Success();
}

Error FramePointerUnwinder::ReadNextFpAndSp(Memory* stack, uint64_t& fp, uint64_t& next_fp,
                                            uint64_t& next_pc, Module::AddressSize pointer_size) {
  switch (pointer_size) {
    case Module::AddressSize::k32Bit: {
      // Read 32 bit integers from the stack and upcast them to 64 bit integers for the caller.
      uint32_t next_fp32;
      uint32_t next_pc32;
      if (auto err = stack->ReadAndAdvance(fp, next_fp32); err.has_err()) {
        return err;
      }
      // Don't check the range of next_fp, because it may not be used as the frame pointer.

      if (auto err = stack->ReadAndAdvance(fp, next_pc32); err.has_err()) {
        return err;
      }

      next_fp = next_fp32;
      next_pc = next_pc32;
      break;
    }
    case Module::AddressSize::k64Bit: {
      // Can just read directly into the caller-provided integers.
      if (auto err = stack->ReadAndAdvance(fp, next_fp); err.has_err()) {
        return err;
      }
      // Don't check the range of next_fp, because it may not be used as the frame pointer.

      if (auto err = stack->ReadAndAdvance(fp, next_pc); err.has_err()) {
        return err;
      }
      break;
    }
    default:
      return Error("Unknown pointer size!");
  }

  if (!cfi_unwinder_->IsValidPC(next_pc)) {
    return Error("next PC %#" PRIx64 " is not pointing to any code", next_pc);
  }

  return Success();
}

}  // namespace unwinder
