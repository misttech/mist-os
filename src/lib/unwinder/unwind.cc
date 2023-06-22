// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/unwind.h"

#include <cinttypes>
#include <cstdint>
#include <cstdio>
#include <set>
#include <unordered_map>
#include <utility>
#include <vector>

#include "src/lib/unwinder/cfi_unwinder.h"
#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/fp_unwinder.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/module.h"
#include "src/lib/unwinder/plt_unwinder.h"
#include "src/lib/unwinder/registers.h"
#include "src/lib/unwinder/scs_unwinder.h"

namespace unwinder {

namespace {

bool PcIsReturnAddress(const Registers& regs) {
  // If |regs| is recovered from a regular function call, rax/lr/ra will be scratched.
  // Otherwise, they will be available.
  RegisterID reg_id;
  switch (regs.arch()) {
    case Registers::Arch::kX64:
      reg_id = RegisterID::kX64_rax;
      break;
    case Registers::Arch::kArm64:
      reg_id = RegisterID::kArm64_lr;
      break;
    case Registers::Arch::kRiscv64:
      reg_id = RegisterID::kRiscv64_ra;
      break;
  }
  uint64_t val;
  return regs.Get(reg_id, val).has_err();
}

}  // namespace

std::string Frame::Describe() const {
  std::string res = "registers={" + regs.Describe() + "}  trust=";
  switch (trust) {
    case Trust::kScan:
      res += "Scan";
      break;
    case Trust::kSCS:
      res += "SCS";
      break;
    case Trust::kFP:
      res += "FP";
      break;
    case Trust::kPLT:
      res += "PLT";
      break;
    case Trust::kCFI:
      res += "CFI";
      break;
    case Trust::kContext:
      res += "Context";
      break;
  }
  if (pc_is_return_address) {
    res += "  pc_is_return_address";
  }
  if (error.has_err()) {
    res += "  error=\"" + error.msg() + "\"";
  }
  return res;
}

Unwinder::Unwinder(const std::vector<Module>& modules) : cfi_unwinder_(modules) {}

std::vector<Frame> Unwinder::Unwind(Memory* stack, const Registers& registers, size_t max_depth) {
  UnavailableMemory unavailable_memory;
  if (!stack) {
    stack = &unavailable_memory;
  }

  std::vector<Frame> res = {{registers, false, Frame::Trust::kContext}};

  while (--max_depth) {
    Frame& current = res.back();

    Frame next(Registers(current.regs.arch()), /*placeholders*/ true, Frame::Trust::kCFI);

    Step(stack, current, next);

    // An undefined PC (e.g. on Linux) or 0 PC (e.g. on Fuchsia) marks the end of the unwinding.
    // Don't include this in the output because it's not a real frame and provides no information.
    // A failed unwinding will also end up with an undefined PC.
    if (uint64_t pc; next.regs.GetPC(pc).has_err() || pc == 0) {
      break;
    }

    res.push_back(std::move(next));
  }

  return res;
}

void Unwinder::Step(Memory* stack, Frame& current, Frame& next) {
  FramePointerUnwinder fp_unwinder(&cfi_unwinder_);
  PltUnwinder plt_unwinder(&cfi_unwinder_);
  ShadowCallStackUnwinder scs_unwinder;

  bool success = false;
  std::string err_msg;

  // Try CFI first because it's the most accurate one.
  if (auto err = cfi_unwinder_.Step(stack, current.regs, next.regs, current.pc_is_return_address);
      err.ok()) {
    next.trust = Frame::Trust::kCFI;
    next.pc_is_return_address = PcIsReturnAddress(next.regs);
    success = true;
  } else {
    err_msg = "CFI: " + err.msg();
  }

  if (!success && !current.pc_is_return_address) {
    // PLT unwinder only works for the first frame.
    if (auto err = plt_unwinder.Step(stack, current.regs, next.regs); err.ok()) {
      next.trust = Frame::Trust::kPLT;
      next.pc_is_return_address = true;
      success = true;
    } else {
      err_msg += "; PLT: " + err.msg();
    }
  }

  // Try frame pointers before SCS because it plays well with the CFI.
  if (!success) {
    if (auto err = fp_unwinder.Step(stack, current.regs, next.regs); err.ok()) {
      next.trust = Frame::Trust::kFP;
      next.pc_is_return_address = true;
      success = true;
    } else {
      err_msg += "; FP: " + err.msg();
    }
  }

  // Try shadow call stacks last because it can only recover PC.
  if (!success) {
    if (auto err = scs_unwinder.Step(stack, current.regs, next.regs); err.ok()) {
      next.trust = Frame::Trust::kSCS;
      next.pc_is_return_address = true;
      success = true;
    } else {
      err_msg += "; SCS: " + err.msg();
    }
  }

  current.fatal_error = !success;
  if (!err_msg.empty()) {
    current.error = Error(err_msg);
  }
}

std::vector<Frame> Unwind(Memory* memory, const std::vector<uint64_t>& modules,
                          const Registers& registers, size_t max_depth) {
  std::vector<Module> converted;
  converted.reserve(modules.size());
  for (const auto& addr : modules) {
    converted.emplace_back(addr, memory, Module::AddressMode::kProcess);
  }
  return Unwinder(converted).Unwind(memory, registers, max_depth);
}

}  // namespace unwinder
