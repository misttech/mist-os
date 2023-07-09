// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>

#include <dev/hw_watchdog/generic32/init.h>
#include <dev/init.h>
#include <dev/uart/dw8250/init.h>
#include <ktl/type_traits.h>
#include <ktl/variant.h>
#include <phys/arch/arch-handoff.h>

#include <ktl/enforce.h>

// TODO-rvbringup: move these into header files
extern void PLICInitEarly(const zbi_dcfg_riscv_plic_driver_t& config);
extern void PLICInitLate();
extern void riscv_generic_timer_init_early(const zbi_dcfg_riscv_generic_timer_driver_t& config);

void PlatformDriverHandoffEarly(const ArchPhysHandoff& arch_handoff) {
  if (arch_handoff.plic_driver) {
    PLICInitEarly(arch_handoff.plic_driver.value());
  }
  if (arch_handoff.generic_timer_driver) {
    riscv_generic_timer_init_early(arch_handoff.generic_timer_driver.value());
  }
}

void PlatformDriverHandoffLate(const ArchPhysHandoff& arch_handoff) {
  if (arch_handoff.plic_driver) {
    PLICInitLate();
  }
}

namespace {

// Overloads for early UART initialization below.
void UartInitEarly(uint32_t extra, const uart::null::Driver::config_type& config) {}

void UartInitEarly(uint32_t extra, const zbi_dcfg_simple_t& config) {
  switch (extra) {
    case ZBI_KERNEL_DRIVER_I8250_MMIO8_UART:
      // TODO-rvbringup: pull stride out of the config entry, but for now hard
      // code that the register stride is 1 byte.
      Dw8250UartInitEarly(config, 1);
      break;
  }
}

void UartInitLate(uint32_t extra) {
  switch (extra) {
    case ZBI_KERNEL_DRIVER_I8250_MMIO8_UART:
      Dw8250UartInitLate();
      break;
  }
}

}  // namespace

void PlatformUartDriverHandoffEarly(const uart::all::Driver& serial) {
  if (gBootOptions->experimental_serial_migration) {
    return;
  }
  ktl::visit([](const auto& uart) { UartInitEarly(uart.extra(), uart.config()); }, serial);
}

void PlatformUartDriverHandoffLate(const uart::all::Driver& serial) {
  if (gBootOptions->experimental_serial_migration) {
    return;
  }
  ktl::visit([](const auto& uart) { UartInitLate(uart.extra()); }, serial);
}
