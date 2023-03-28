// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <zircon/boot/driver-config.h>

#include <cstdint>

#include <arch/defines.h>
#include <arch/x86/apic.h>
#include <ktl/optional.h>
#include <platform/uart.h>
#include <vm/physmap.h>

#include "memory.h"

volatile void* PlatformUartMapMmio(paddr_t paddr) {
  volatile void* vaddr = paddr_to_physmap(paddr);
  mark_mmio_region_to_reserve(reinterpret_cast<vaddr_t>(vaddr), PAGE_SIZE);
  return vaddr;
}

PlatformUartIoProvider<zbi_dcfg_simple_pio_t>::PlatformUartIoProvider(
    const zbi_dcfg_simple_pio_t& config, uint16_t pio_size)
    : Base(config, pio_size) {
  // Reserve pio.
  mark_pio_region_to_reserve(config.base, 8);
}

ktl::optional<uint32_t> PlatformUartGetIrqNumber(uint32_t irq_num) {
  if (irq_num == 0) {
    return ktl::nullopt;
  }
  return apic_io_isa_to_global(static_cast<uint8_t>(irq_num));
}
