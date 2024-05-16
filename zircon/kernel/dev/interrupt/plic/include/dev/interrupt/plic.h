// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_DEV_INTERRUPT_PLIC_INCLUDE_DEV_INTERRUPT_PLIC_H_
#define ZIRCON_KERNEL_DEV_INTERRUPT_PLIC_INCLUDE_DEV_INTERRUPT_PLIC_H_

#include <lib/zbi-format/driver-config.h>

// Early and late initialization routines for the driver.
void PLICInitEarly(const zbi_dcfg_riscv_plic_driver_t& config);
void PLICInitLate();

#endif
