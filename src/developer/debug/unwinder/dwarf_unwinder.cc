// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/unwinder/dwarf_unwinder.h"

#include <cinttypes>
#include <cstdint>
#include <vector>

namespace unwinder {

DwarfUnwinder::DwarfUnwinder(const std::vector<Module>& modules) {
  for (const auto& module : modules) {
    module_map_.emplace(module.load_address, module);
  }
}

Error DwarfUnwinder::Step(Memory* stack, Registers current, Registers& next,
                          bool is_return_address) {
  uint64_t pc;
  if (auto err = current.GetPC(pc); err.has_err()) {
    return err;
  }

  // is_return_address indicates whether pc in the current registers is a return address from a
  // previous "Step". If it is, we need to subtract 1 to find the call site because "call" could
  // be the last instruction of a nonreturn function and now the PC is pointing outside of the
  // valid code boundary.
  //
  // Subtracting 1 is sufficient here because in DwarfCfiParser::ParseInstructions, we scan CFI
  // until pc > pc_limit. So it's still correct even if pc_limit is not pointing to the beginning
  // of an instruction.
  if (is_return_address) {
    pc -= 1;
    current.SetPC(pc);
  }

  auto module_it = module_map_.upper_bound(pc);
  if (module_it == module_map_.begin()) {
    return Error("%#" PRIx64 " is not covered by any module", pc);
  }
  module_it--;
  uint64_t module_address = module_it->first;
  Module& module = module_it->second;

  auto cfi_it = cfi_map_.find(module_address);
  if (cfi_it == cfi_map_.end()) {
    DwarfCfi dwarf_cfi(module.memory, module_address, module.mode);
    cfi_it = cfi_map_.emplace(module_address, dwarf_cfi).first;
    if (auto err = cfi_it->second.Load(); err.has_err()) {
      return err;
    }
  }
  if (auto err = cfi_it->second.Step(stack, current, next); err.has_err()) {
    return err;
  }
  return Success();
}

}  // namespace unwinder
