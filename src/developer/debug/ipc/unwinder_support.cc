// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/ipc/unwinder_support.h"

#include "src/developer/debug/shared/arch.h"
#include "src/developer/debug/shared/register_info.h"

namespace debug_ipc {

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
        FX_NOTIMPLEMENTED();
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
