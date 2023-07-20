// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <zircon/assert.h>

#include <phys/arch/arch-phys-info.h>
#include <phys/main.h>

#include "handoff-prep.h"

#include <ktl/enforce.h>

void HandoffPrep::ArchHandoff() {
  ZX_DEBUG_ASSERT(handoff_);
  ArchPhysHandoff& arch_handoff = handoff_->arch_handoff;

  arch_handoff.boot_hart_id = gArchPhysInfo->boot_hart_id;
}

void HandoffPrep::ArchSummarizeMiscZbiItem(const zbi_header_t& header,
                                           ktl::span<const ktl::byte> payload) {
  ZX_DEBUG_ASSERT(handoff_);
  ArchPhysHandoff& arch_handoff = handoff_->arch_handoff;

  switch (header.type) {
    case ZBI_TYPE_KERNEL_DRIVER: {
      switch (header.extra) {
        case ZBI_KERNEL_DRIVER_RISCV_PLIC:
          ZX_ASSERT(payload.size() >= sizeof(zbi_dcfg_riscv_plic_driver_t));
          arch_handoff.plic_driver =
              *reinterpret_cast<const zbi_dcfg_riscv_plic_driver_t*>(payload.data());
          SaveForMexec(header, payload);
          break;
        case ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER:
          ZX_ASSERT(payload.size() >= sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
          arch_handoff.generic_timer_driver =
              *reinterpret_cast<const zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
          SaveForMexec(header, payload);
          break;
      }
      break;
    }
  }
}
