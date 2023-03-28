// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PLATFORM_INCLUDE_PLATFORM_UART_H_
#define ZIRCON_KERNEL_PLATFORM_INCLUDE_PLATFORM_UART_H_

#include <lib/uart/null.h>
#include <lib/uart/uart.h>
#include <zircon/boot/driver-config.h>

#include <ktl/optional.h>

// Returns the virtual address of the |base_addr| used in the uart.
// Any memory related book keeping with this mapping take place here.
volatile void* PlatformUartMapMmio(paddr_t base_addr);

// Returns the |irq| number to be used for registering an IRQ Handler if such |irq_num| can be
// translated.
// Returns |nullopt| if there is no irq wired to the provided |irq_num|, if the provided
// |irq_num| has a platform specific meaning.
ktl::optional<uint32_t> PlatformUartGetIrqNumber(uint32_t irq_num);

// ulib/uart IoProvider implementation for the kernel.
template <typename Config>
class PlatformUartIoProvider;

// Null Driver specialization.
template <>
class PlatformUartIoProvider<uart::null::Driver::config_type>
    : public uart::BasicIoProvider<uart::null::Driver::config_type> {
 public:
  using Base = uart::BasicIoProvider<uart::null::Driver::config_type>;
  using Base::Base;
};

// MMIO Driver specialization.
template <>
class PlatformUartIoProvider<zbi_dcfg_simple_t> : public uart::BasicIoProvider<zbi_dcfg_simple_t> {
 public:
  using Base = uart::BasicIoProvider<zbi_dcfg_simple_t>;

  using Base::Base;
  PlatformUartIoProvider(const zbi_dcfg_simple_t& config, uint16_t pio_size)
      : Base(config, pio_size, PlatformUartMapMmio) {}
};

#if defined(__x86_64__) || defined(__i386__)
// PIO Driver implementation.
template <>
class PlatformUartIoProvider<zbi_dcfg_simple_pio_t>
    : public uart::BasicIoProvider<zbi_dcfg_simple_pio_t> {
 public:
  using Base = uart::BasicIoProvider<zbi_dcfg_simple_pio_t>;

  using Base::Base;
  PlatformUartIoProvider(const zbi_dcfg_simple_pio_t& config, uint16_t pio_size);
};
#endif

#endif  // ZIRCON_KERNEL_PLATFORM_INCLUDE_PLATFORM_UART_H_
