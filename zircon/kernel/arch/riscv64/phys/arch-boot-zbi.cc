// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/zbi-boot.h>
#include <lib/arch/zbi.h>

#include <phys/arch/arch-phys-info.h>
#include <phys/boot-zbi.h>

#include "riscv64.h"

void BootZbi::ZbiBoot(arch::ZbiKernelImage* kernel, void* arg) {
  arch::ZbiBoot(kernel, arg, gArchPhysInfo->boot_hart_id);
}

void BootZbi::ZbiBootRaw(uintptr_t entry, void* data) {
  arch::ZbiBootRaw(entry, data, gArchPhysInfo->boot_hart_id);
}
