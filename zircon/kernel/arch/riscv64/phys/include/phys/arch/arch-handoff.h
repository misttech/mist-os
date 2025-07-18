// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_

// Note: we refrain from using the ktl namespace as <phys/handoff.h> is
// expected to be compiled in the userboot toolchain.

#include <lib/arch/riscv64/feature.h>
#include <lib/zbi-format/driver-config.h>

#include <cstdint>
#include <optional>
#include <span>

struct ArchPatchInfo {};

struct RiscvPlicDriverConfig {
  zbi_dcfg_riscv_plic_driver_t zbi{};
  std::span<volatile std::byte> mmio;
};

// This holds (or points to) all riscv64-specific data that is handed off from
// physboot to the kernel proper at boot time.
struct ArchPhysHandoff {
  uint64_t boot_hart_id;

  // The lowest common denominator of all supported features/extensions across
  // all harts.
  arch::RiscvFeatures cpu_features;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_RISCV_PLIC) payload.
  std::optional<RiscvPlicDriverConfig> plic_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) payload.
  std::optional<zbi_dcfg_riscv_generic_timer_driver_t> generic_timer_driver;
};

// TODO(https://fxbug.dev/42164859): This is an arbitrary address in the upper half of
// sv39.  It must match what the kernel's page-table bootstrapping actually
// uses as the virtual address of the kernel load image.
inline constexpr uint64_t kArchHandoffVirtualAddress = 0xffffffff00000000;  // -4GB

// TODO(https://fxbug.dev/42164859): Make this constant the source of truth
// for the physmap in the kernel.
inline constexpr uint64_t kArchPhysmapVirtualBase = 0xffff'ffc0'0000'0000;

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
