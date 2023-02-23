// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_PHYS_RISCV64_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_PHYS_RISCV64_H_

#include <stdint.h>

// This is set by start.S from the value passed in by the boot loader / SBI.
extern uint64_t gBootHartId;

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_PHYS_RISCV64_H_
