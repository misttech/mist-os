// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_UART_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_UART_H_

#include <lib/boot-options/boot-options.h>
#include <lib/memalloc/range.h>
#include <lib/uart/all.h>
#include <lib/zbitl/view.h>

using UartDriver = uart::all::KernelDriver<uart::BasicIoProvider, uart::UnsynchronizedPolicy>;

UartDriver& GetUartDriver();

// Wires up the associated UART to stdout via PhysConsole::set_serial.
void SetUartConsole(const uart::all::Driver& uart);

// Obtain a `page_aligned` range for `driver`'s mmio range.
template <typename AllDrivers>
std::optional<memalloc::Range> GetUartMmioRange(const AllDrivers& driver, size_t page_size) {
  std::optional<memalloc::Range> mmio_range;
  uart::internal::Visit(
      [&mmio_range, page_size](const auto& driver) {
        using driver_type = std::decay_t<decltype(driver)>;
        using config_type = std::decay_t<decltype(driver.config())>;
        if constexpr (std::is_same_v<config_type, zbi_dcfg_simple_t>) {
          const zbi_dcfg_simple_t& uart_mmio_config = driver.config();

          mmio_range = {
              .addr = uart_mmio_config.mmio_phys,
              .type = memalloc::Type::kPeripheral,
          };

          switch (driver_type::kIoType) {
            case uart::IoRegisterType::kMmio32:
              mmio_range->size = driver.io_slots();
              break;
            case uart::IoRegisterType::kMmio8:
              mmio_range->size = driver.io_slots() * 4;
              break;

            default:
              ZX_PANIC("Unknown uart::IoRegisterType for %.*s/n",
                       static_cast<int>(driver_type::kConfigName.length()),
                       driver_type::kConfigName.data());
          }

          // Adjust range to page boundaries.
          ZX_ASSERT(mmio_range);
          uint64_t addr = fbl::round_down<uint64_t>(mmio_range->addr, page_size);
          mmio_range->addr = addr;
          mmio_range->size =
              fbl::round_up<uint64_t>(mmio_range->addr + mmio_range->size, page_size) - addr;
        }
      },
      driver);

  return mmio_range;
}

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_UART_H_
