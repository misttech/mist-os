// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/unwinder/unwind.h"

#include <cinttypes>
#include <cstdint>
#include <cstdio>
#include <set>
#include <unordered_map>
#include <utility>
#include <vector>

#include "src/developer/debug/unwinder/dwarf_unwinder.h"
#include "src/developer/debug/unwinder/error.h"
#include "src/developer/debug/unwinder/memory.h"
#include "src/developer/debug/unwinder/module.h"
#include "src/developer/debug/unwinder/registers.h"
#include "src/developer/debug/unwinder/scs_unwinder.h"

namespace unwinder {

std::string Frame::Describe() const {
  std::string res = "registers={" + regs.Describe() + "}  trust=";
  switch (trust) {
    case Trust::kScan:
      res += "Scan";
      break;
    case Trust::kFP:
      res += "FP";
      break;
    case Trust::kSCS:
      res += "SCS";
      break;
    case Trust::kCFI:
      res += "CFI";
      break;
    case Trust::kContext:
      res += "Context";
      break;
  }
  if (error.has_err()) {
    res += "  error=\"" + error.msg() + "\"";
  }
  return res;
}

Unwinder::Unwinder(const std::vector<Module>& modules) : dwarf_unwinder_(modules) {}

std::vector<Frame> Unwinder::Unwind(Memory* stack, const Registers& registers, size_t max_depth) {
  UnavailableMemory unavailable_memory;
  if (!stack) {
    stack = &unavailable_memory;
  }

  std::vector<Frame> res = {{registers, Frame::Trust::kContext, Success()}};
  ShadowCallStackUnwinder scs_unwinder(stack);

  while (--max_depth) {
    Registers next(registers.arch());
    Frame::Trust trust;

    Frame& current = res.back();
    trust = Frame::Trust::kCFI;
    current.error =
        dwarf_unwinder_.Step(stack, current.regs, next, current.trust != Frame::Trust::kContext);

    if (current.error.has_err() && scs_unwinder.Step(current.regs, next).ok()) {
      trust = Frame::Trust::kSCS;
      current.error = Success();
    }

    if (current.error.has_err()) {
      break;
    }

    // An undefined PC (e.g. on Linux) or 0 PC (e.g. on Fuchsia) marks the end of the unwinding.
    // Don't include this in the output because it's not a real frame and provides no information.
    if (uint64_t pc; next.GetPC(pc).has_err() || pc == 0) {
      break;
    }

    res.emplace_back(std::move(next), trust, Success());
  }

  return res;
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
