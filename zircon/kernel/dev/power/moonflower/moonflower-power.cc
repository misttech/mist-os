// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include <lib/ddk/platform-defs.h>
#include <lib/mmio-ptr/mmio-ptr.h>
#include <lib/zbi-format/driver-config.h>
#include <trace.h>
#include <zircon/types.h>

#include <dev/power.h>
#include <dev/power/moonflower/init.h>
#include <dev/psci.h>
#include <lk/init.h>
#include <pdev/power.h>
#include <phys/handoff.h>
#include <vm/physmap.h>

#define LOCAL_TRACE 0

namespace {

// Reboot modes, as understood by the moonflower bootloader
// These need to be written to spmi-sdam nvmem cell restart_reason
enum class MOONFLOWER_REBOOT_MODE : uint8_t {
  NORMAL = 0,
  RECOVERY = 1,
  FASTBOOT = 2,
  RTC = 3,
  DMV_CORRUPT = 4,
  DMV_ENFORCING = 5,
  KEYS_CLEAR = 6,
  SHIP_MODE = 32,
};

zx_status_t moonflower_power_reboot(power_reboot_flags flags) {
  LTRACEF("flags %#x\n", static_cast<uint32_t>(flags));

  // Set the moonflower bootloader recovery mode
  [[maybe_unused]] MOONFLOWER_REBOOT_MODE mode = MOONFLOWER_REBOOT_MODE::NORMAL;
  switch (flags) {
    case power_reboot_flags::REBOOT_NORMAL:
      mode = MOONFLOWER_REBOOT_MODE::NORMAL;
      break;
    case power_reboot_flags::REBOOT_BOOTLOADER:
      mode = MOONFLOWER_REBOOT_MODE::FASTBOOT;
      break;
    case power_reboot_flags::REBOOT_RECOVERY:
      mode = MOONFLOWER_REBOOT_MODE::RECOVERY;
      break;
  }

  // TODO(https://fxbug.dev//383788491): Set reboot reason in the populate nvmem here.

  // Hit the reboot switch
  // Call through to SYSTEM_RESET2 with a vendor specific reset type (bit 31 set).
  // TODO(drewry, travisg): figure out if the reboot flags above can be combined to
  // influence this at all.
  psci_system_reset2_raw(0x80000000, 0);
  for (;;) {
    __wfi();
  }

  return ZX_ERR_NOT_SUPPORTED;
}

// Set up standard pdev power looks except for reboot, which needs to tweak the
// arguments passed to the PSCI reboot call
constexpr pdev_power_ops moonflower_power_ops = {
    .reboot = moonflower_power_reboot,
    .shutdown = psci_system_off,
    .cpu_off = psci_cpu_off,
    .cpu_on = psci_cpu_on,
    .get_cpu_state = psci_get_cpu_state,
};

}  // anonymous namespace

void moonflower_power_init_early() {
  dprintf(INFO, "POWER: registering moonflower power hooks\n");
  pdev_register_power(&moonflower_power_ops);
}
