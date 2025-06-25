// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/cfi_unwinder.h"

#include <cinttypes>
#include <cstdint>
#include <memory>
#include <utility>
#include <vector>

#include "src/lib/unwinder/cfi_module.h"
#include "src/lib/unwinder/error.h"

namespace unwinder {

bool CfiModuleInfo::IsValidPC(uint64_t pc) const {
  return ((binary && binary->IsValidPC(pc)) || (debug_info && debug_info->IsValidPC(pc)));
}

CfiUnwinder::CfiUnwinder(const std::vector<Module>& modules) : UnwinderBase(this) {
  for (const auto& module : modules) {
    module_map_.emplace(module.load_address,
                        CfiModuleInfo{.module = module, .binary = nullptr, .debug_info = nullptr});
  }
}

Error CfiUnwinder::Step(Memory* stack, const Frame& current, Frame& next) {
  return Step(stack, current.regs, next.regs, current.pc_is_return_address);
}

Error CfiUnwinder::Step(Memory* stack, const Registers& current, Registers& next,
                        bool is_return_address) {
  uint64_t pc;
  if (auto err = current.GetPC(pc); err.has_err()) {
    return err;
  }

  Registers regs = current;

  // is_return_address indicates whether pc in the current registers is a return address from a
  // previous "Step". If it is, we need to subtract 1 to find the call site because "call" could
  // be the last instruction of a nonreturn function and now the PC is pointing outside of the
  // valid code boundary.
  //
  // Subtracting 1 is sufficient here because in CfiParser::ParseInstructions, we scan CFI until
  // pc > pc_limit. So it's still correct even if pc_limit is not pointing to the beginning of an
  // instruction.
  if (is_return_address) {
    pc -= 1;
    regs.SetPC(pc);
  }

  CfiModuleInfo* cfi;
  if (auto err = GetCfiModuleInfoForPc(pc, &cfi); err.has_err()) {
    return err;
  }

  if (auto err = cfi->binary->Step(stack, regs, next); err.has_err()) {
    return err;
  }
  return Success();
}

void CfiUnwinder::AsyncStep(AsyncMemory* stack, const Frame& current,
                            fit::callback<void(Error, Registers)> cb) {
  // TODO(https://fxbug.dev/316047562): Make CFI work on RISC-V.
  if (current.regs.arch() == Registers::Arch::kRiscv64) {
    return cb(Error("RISC-V is not supported with the CFI Unwinder."),
              Registers(current.regs.arch()));
  }

  AsyncStep(stack, current.regs, current.pc_is_return_address, std::move(cb));
}

void CfiUnwinder::AsyncStep(AsyncMemory* stack, Registers current, bool is_return_address,
                            fit::callback<void(Error, Registers)> cb) {
  uint64_t pc;
  if (auto err = current.GetPC(pc); err.has_err()) {
    return cb(err, Registers(current.arch()));
  }

  // is_return_address indicates whether pc in the current registers is a return address from a
  // previous "Step". If it is, we need to subtract 1 to find the call site because "call" could
  // be the last instruction of a nonreturn function and now the PC is pointing outside of the
  // valid code boundary.
  //
  // Subtracting 1 is sufficient here because in CfiParser::ParseInstructions, we scan CFI until
  // pc > pc_limit. So it's still correct even if pc_limit is not pointing to the beginning of an
  // instruction.
  if (is_return_address) {
    pc -= 1;
    current.SetPC(pc);
  }

  CfiModuleInfo* cfi_info;
  if (auto err = GetCfiModuleInfoForPc(pc, &cfi_info); err.has_err()) {
    return cb(err, Registers(current.arch()));
  }

  if (cfi_info->debug_info) {
    // Try stepping with the debug_info if it is available. This could contain both .debug_frame and
    // .eh_frame sections in the case of a fully unstripped binary, or just a .debug_frame section
    // in the case of a separated debug_info binary. Both have to fail for us to try again with the
    // "binary" file, which will only contain an .eh_frame section.
    cfi_info->debug_info->AsyncStep(
        stack, current,
        [cfi_info, stack, current, cb = std::move(cb)](Error err, Registers regs) mutable {
          if (err.has_err()) {
            // debug_info didn't work, try again with the binary module instead. If this fails it's
            // a fatal error for this unwinder.
            if (cfi_info->binary) {
              return cfi_info->binary->AsyncStep(
                  stack, current, [e = err, cb = std::move(cb)](Error err, Registers regs) mutable {
                    if (err.has_err()) {
                      // Propagate both errors up.
                      return cb(Error("debug_info:" + e.msg() + ";binary:" + err.msg()),
                                std::move(regs));
                    }

                    // Using the binary worked.
                    cb(err, std::move(regs));
                  });
            } else {
              return cb(Error("debug_info:" + err.msg() + ";binary not present."), regs);
            }
          }

          // Unwinding with the debug_info module worked, issue the callback.
          cb(err, std::move(regs));
        });
  } else if (cfi_info->binary) {
    // No debug_info available, unwind with the binary module.
    cfi_info->binary->AsyncStep(stack, current, std::move(cb));
  } else {
    return cb(Error("Module has no associated memory."), Registers(current.arch()));
  }
}

bool CfiUnwinder::IsValidPC(uint64_t pc) {
  CfiModuleInfo* cfi;
  return GetCfiModuleInfoForPc(pc, &cfi).ok();
}

Error CfiUnwinder::GetCfiModuleInfoForPc(uint64_t pc, CfiModuleInfo** out) {
  auto module_it = module_map_.upper_bound(pc);
  if (module_it == module_map_.begin()) {
    return Error("%#" PRIx64 " is not covered by any module", pc);
  }
  module_it--;
  uint64_t module_address = module_it->first;
  auto& module_info = module_it->second;

  if (module_info.module.binary_memory) {
    module_info.binary = std::make_unique<CfiModule>(module_info.module.binary_memory,
                                                     module_address, module_info.module.mode);
    // Loading the main binary file should always contain either an eh_frame section or a
    // debug_frame section.
    if (auto err = module_info.binary->Load(); err.has_err()) {
      return err;
    }
  }

  if (module_info.module.debug_info_memory) {
    module_info.debug_info = std::make_unique<CfiModule>(module_info.module.debug_info_memory,
                                                         module_address, module_info.module.mode);
    // A split debug info file may contain neither eh_frame nor debug_frame sections, it is not an
    // error if this fails to load.
    if (auto err = module_info.debug_info->Load(); err.has_err()) {
      // Reset the pointer to null to indicate that it should not be used for look ups later.
      module_info.debug_info.reset();
    }
  }

  if (!module_info.IsValidPC(pc)) {
    return Error("%#" PRIx64 " is not a valid PC in module %#" PRIx64, pc, module_address);
  }

  *out = &module_info;
  return Success();
}

}  // namespace unwinder
