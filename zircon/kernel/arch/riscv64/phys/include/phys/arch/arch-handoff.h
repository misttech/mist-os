// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_

#include <stdint.h>
#include <zircon/boot/driver-config.h>

#include <ktl/optional.h>
#include <ktl/variant.h>

// This holds (or points to) all riscv64-specific data that is handed off from
// physboot to the kernel proper at boot time.
struct ArchPhysHandoff {
  uint64_t boot_hart_id;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_RISCV_PLIC) payload.
  ktl::optional<zbi_dcfg_riscv_plic_driver_t> plic_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) payload.
  ktl::optional<zbi_dcfg_riscv_generic_timer_driver_t> generic_timer_driver;
};

// TODO(fxbug.dev/84107): This is an arbitrary address in the upper half of
// sv39.  It must match what the kernel's page-table bootstrapping actually
// uses as the virtual address of the kernel load image.
inline constexpr uint64_t kArchHandoffVirtualAddress = 0xffffffc000000000;

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
