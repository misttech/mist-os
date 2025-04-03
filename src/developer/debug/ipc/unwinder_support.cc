// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/ipc/unwinder_support.h"

#include "src/developer/debug/shared/arch.h"
#include "src/developer/debug/shared/register_info.h"
#include "src/lib/unwinder/unwind.h"

namespace debug_ipc {

unwinder::Registers ConvertRegisters(debug::Arch arch,
                                     const std::vector<debug::RegisterValue>& regs) {
  std::optional<unwinder::Registers> res = std::nullopt;

  switch (arch) {
    case debug::Arch::kX64:
      res = unwinder::Registers(unwinder::Registers::Arch::kX64);
      // The first 4 registers are out-of-order.
      res->Set(unwinder::RegisterID::kX64_rax, static_cast<uint64_t>((regs)[0].GetValue()));
      res->Set(unwinder::RegisterID::kX64_rbx, static_cast<uint64_t>((regs)[1].GetValue()));
      res->Set(unwinder::RegisterID::kX64_rcx, static_cast<uint64_t>((regs)[2].GetValue()));
      res->Set(unwinder::RegisterID::kX64_rdx, static_cast<uint64_t>((regs)[3].GetValue()));
      for (int i = 4; i < static_cast<int>(unwinder::RegisterID::kX64_last); i++) {
        res->Set(static_cast<unwinder::RegisterID>(i), static_cast<uint64_t>((regs)[i].GetValue()));
      }
      break;
    case debug::Arch::kArm64:
      res = unwinder::Registers(unwinder::Registers::Arch::kArm64);
      for (int i = 0; i < static_cast<int>(unwinder::RegisterID::kArm64_last); i++) {
        res->Set(static_cast<unwinder::RegisterID>(i), static_cast<uint64_t>((regs)[i].GetValue()));
      }
      break;
    case debug::Arch::kRiscv64:
      FX_NOTREACHED() << "Riscv64 is not supported yet";
      break;
    default:
      FX_NOTREACHED() << "Unknown platform";
      break;
  }

  return *res;
}

std::vector<debug_ipc::StackFrame> ConvertFrames(const std::vector<unwinder::Frame>& frames) {
  std::vector<debug_ipc::StackFrame> res;

  for (const unwinder::Frame& frame : frames) {
    std::vector<debug::RegisterValue> frame_regs;
    uint64_t ip = 0;
    uint64_t sp = 0;
    frame.regs.GetSP(sp);
    frame.regs.GetPC(ip);
    if (!res.empty()) {
      res.back().cfa = sp;
    }
    debug::Arch arch;
    switch (frame.regs.arch()) {
      case unwinder::Registers::Arch::kX64:
        arch = debug::Arch::kX64;
        break;
      case unwinder::Registers::Arch::kArm64:
        arch = debug::Arch::kArm64;
        break;
      case unwinder::Registers::Arch::kRiscv64:
        arch = debug::Arch::kRiscv64;
        break;
    }
    frame_regs.reserve(frame.regs.size());
    for (auto& [reg_id, val] : frame.regs) {
      if (auto* info = debug::DWARFToRegisterInfo(arch, static_cast<uint32_t>(reg_id))) {
        frame_regs.emplace_back(info->id, val);
      }
    }

    res.emplace_back(ip, sp, 0, frame_regs);
  }

  return res;
}

}  // namespace debug_ipc
