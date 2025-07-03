// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_DEV_INTERRUPT_PLIC_INCLUDE_DEV_INTERRUPT_PLIC_H_
#define ZIRCON_KERNEL_DEV_INTERRUPT_PLIC_INCLUDE_DEV_INTERRUPT_PLIC_H_

#include <phys/handoff.h>

// Early and late initialization routines for the driver.
void PLICInitEarly(const RiscvPlicDriverConfig& config);
void PLICInitLate();

#endif  // ZIRCON_KERNEL_DEV_INTERRUPT_PLIC_INCLUDE_DEV_INTERRUPT_PLIC_H_
