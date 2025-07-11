// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/arm_ehabi_unwinder.h"

#include "src/lib/unwinder/arm_ehabi_module.h"
#include "src/lib/unwinder/cfi_unwinder.h"
#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/module.h"

namespace unwinder {

Error ArmEhAbiUnwinder::Step(Memory* stack, const Frame& current, Frame& next) {
  if (current.regs.arch() != Registers::Arch::kArm64 &&
      current.regs.arch() != Registers::Arch::kArm32) {
    return Error("Not ARM architecture.");
  }

  uint64_t pc = 0;
  auto e = current.regs.GetPC(pc);

  CfiModuleInfo* info;
  if (auto e = cfi_unwinder_->GetCfiModuleInfoForPc(pc, &info); e.has_err()) {
    return e;
  }

  switch (info->module.size) {
    case Module::AddressSize::k32Bit:
      next.regs = Registers(Registers::Arch::kArm32);
      return Step(stack, info, current.regs, next.regs);
    case Module::AddressSize::k64Bit:
      return Error("Module for PC is not 32 bit.");
    default:
      return Error("Unknown ELF Class");
  }
}

Error ArmEhAbiUnwinder::Step(Memory* stack, CfiModuleInfo* info, const Registers& current,
                             Registers& next) {
  ArmEhAbiModule* ehabi_module = nullptr;
  if (auto e = GetEhAbiModuleFromModuleInfo(info, &ehabi_module); e.has_err()) {
    return e;
  }

  return ehabi_module->Step(stack, current, next);
}

void ArmEhAbiUnwinder::AsyncStep(AsyncMemory* stack, const Frame& current,
                                 fit::callback<void(Error, Registers)> cb) {
  return cb(Error("Not implemented yet."), Registers(current.regs.arch()));
}

void ArmEhAbiUnwinder::AsyncStep(AsyncMemory* stack, Registers current, bool is_return_address,
                                 fit::callback<void(Error, Registers)> cb) {
  // Shouldn't reach here.
  return cb(Error("Not implemented yet."), Registers(current.arch()));
}

Error ArmEhAbiUnwinder::GetEhAbiModuleFromModuleInfo(CfiModuleInfo* info, ArmEhAbiModule** out) {
  // The CFI Unwinder keeps a record of all the modules, so it can properly find the right module
  // for this PC. Since we don't have to keep track of anything other than the 32 bit modules here
  // we can just index on the load address of the already found module.
  auto it = module_map_.find(static_cast<uint32_t>(info->module.load_address));

  if (it == module_map_.end()) {
    // Need to insert this module.
    auto insert_pair = module_map_.insert(std::make_pair(
        info->module.load_address,
        std::make_unique<ArmEhAbiModule>(info->module.binary_memory, info->module.load_address)));

    it = insert_pair.first;
  }

  ArmEhAbiModule* ehabi_module = it->second.get();
  if (auto err = ehabi_module->Load(); err.has_err()) {
    return err;
  }

  *out = ehabi_module;
  return Success();
}

}  // namespace unwinder
