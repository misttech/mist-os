// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_DEV_UART_DW8250_INCLUDE_DEV_UART_DW8250_INIT_H_
#define ZIRCON_KERNEL_DEV_UART_DW8250_INCLUDE_DEV_UART_DW8250_INIT_H_

#include <lib/zbi-format/driver-config.h>
#include <sys/types.h>

// Initialization routines at the PLATFORM_EARLY and PLATFORM levels.
// Stride sets the width of the registers and their offset from each other.
void Dw8250UartInitEarly(const zbi_dcfg_simple_t& config, size_t stride = 4);
void Dw8250UartInitLate();

#endif  // ZIRCON_KERNEL_DEV_UART_DW8250_INCLUDE_DEV_UART_DW8250_INIT_H_
