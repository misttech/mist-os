// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/root_resource_filter.h>
#include <lib/uart/uart.h>
#include <lib/zbi-format/driver-config.h>
#include <zircon/syscalls/resource.h>

#include <cstdint>

#include <arch/defines.h>
#include <arch/x86/apic.h>
#include <ktl/optional.h>
#include <platform/uart.h>
#include <vm/physmap.h>

void PlatformUartPrepareMmio(paddr_t paddr, size_t size) {
  root_resource_filter_add_deny_region(paddr, size, ZX_RSRC_KIND_MMIO);
}

PlatformUartIoProvider<zbi_dcfg_simple_pio_t, uart::IoRegisterType::kPio>::PlatformUartIoProvider(
    const zbi_dcfg_simple_pio_t& config, uint16_t io_slots)
    : Base(config, io_slots) {
  // Reserve pio.
  root_resource_filter_add_deny_region(config.base, io_slots, ZX_RSRC_KIND_IOPORT);
}

ktl::optional<uint32_t> PlatformUartGetIrqNumber(uint32_t irq_num) {
  if (irq_num == 0) {
    return ktl::nullopt;
  }
  return apic_io_isa_to_global(static_cast<uint8_t>(irq_num));
}
