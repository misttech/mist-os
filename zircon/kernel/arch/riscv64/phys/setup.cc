// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/riscv64/system.h>

#include <phys/arch/arch-phys-info.h>
#include <phys/main.h>

#include "riscv64.h"

// The boot_hart_id field is initialized by start.S.
ArchPhysInfo gArchPhysInfoStorage;

ArchPhysInfo* gArchPhysInfo;

void ArchSetUp(void* zbi) {
  gArchPhysInfo = &gArchPhysInfoStorage;

  arch::RiscvStvec::Get()
      .FromValue(0)
      .set_base(reinterpret_cast<uintptr_t>(ArchPhysExceptionEntry))
      .set_mode(arch::RiscvStvec::Mode::kDirect)
      .Write();
}
