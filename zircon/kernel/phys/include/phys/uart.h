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
      [&mmio_range, page_size]<typename UartDriver>(const UartDriver& driver) {
        if constexpr (uart::MmioDriver<UartDriver>) {
          uart::MmioRange uart_mmio = driver.mmio_range();

          mmio_range = {
              .addr = uart_mmio.address,
              .size = uart_mmio.size,
              .type = memalloc::Type::kPeripheral,
          };

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
