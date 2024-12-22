// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_ARM64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
#define ZIRCON_KERNEL_ARCH_ARM64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_

// Note: we refrain from using the ktl namespace as <phys/handoff.h> is
// expected to be compiled in the userboot toolchain.

#include <lib/arch/arm64/smccc.h>
#include <lib/boot-options/arm64.h>
#include <lib/zbi-format/driver-config.h>

#include <optional>
#include <variant>

struct ArchPatchInfo {
  Arm64AlternateVbar alternate_vbar = Arm64AlternateVbar::kNone;
};

struct ZbiAmlogicRng {
  enum class Version {
    kV1,  // ZBI_KERNEL_DRIVER_AMLOGIC_RNG_V1
    kV2,  // ZBI_KERNEL_DRIVER_AMLOGIC_RNG_V2
  };

  zbi_dcfg_amlogic_rng_driver_t config;
  Version version;
};

// This holds (or points to) all arm64-specific data that is handed off from
// physboot to the kernel proper at boot time.
struct ArchPhysHandoff {
  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_AMLOGIC_HDCP) payload.
  std::optional<zbi_dcfg_amlogic_hdcp_driver_t> amlogic_hdcp_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_AMLOGIC_RNG_V1) or
  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_AMLOGIC_RNG_V2) payload
  std::optional<ZbiAmlogicRng> amlogic_rng_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_ARM_GENERIC_TIMER) payload.
  std::optional<zbi_dcfg_arm_generic_timer_driver_t> generic_timer_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_ARM_GIC_V2/ZBI_KERNEL_DRIVER_ARM_GIC_V3) payload.
  std::variant<std::monostate, zbi_dcfg_arm_gic_v2_driver_t, zbi_dcfg_arm_gic_v3_driver_t>
      gic_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_ARM_PSCI) payload.
  std::optional<zbi_dcfg_arm_psci_driver_t> psci_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_GENERIC32_WATCHDOG) payload.
  std::optional<zbi_dcfg_generic32_watchdog_t> generic32_watchdog_driver;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_MOTMOT_POWER) payload.
  bool motmot_power_driver = false;

  // (ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_MOONFLOWER_POWER) payload.
  bool moonflower_power_driver = false;

  // See ArchPatchInfo, above.
  Arm64AlternateVbar alternate_vbar = Arm64AlternateVbar::kNone;
};

inline constexpr uint64_t kArchHandoffVirtualAddress = 0xffffffff10000000;

// Whether a peripheral range for the UART needs to be synthesized.
inline constexpr bool kArchHandoffGenerateUartPeripheralRanges = true;

#endif  // ZIRCON_KERNEL_ARCH_ARM64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
