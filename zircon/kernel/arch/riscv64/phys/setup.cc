// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/riscv64/system.h>

#include <phys/main.h>

#include "riscv64.h"

// Initialized by start.S.
uint64_t gBootHartId;

void ArchSetUp(void* zbi) {
  arch::RiscvStvec::Get()
      .FromValue(0)
      .set_base(reinterpret_cast<uintptr_t>(ArchPhysExceptionEntry))
      .set_mode(arch::RiscvStvec::Mode::kDirect)
      .Write();
}
