// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_DEV_POWER_H_
#define ZIRCON_KERNEL_INCLUDE_DEV_POWER_H_

#include <sys/types.h>
#include <zircon/compiler.h>

enum class power_reboot_flags {
  REBOOT_NORMAL = 0,
  REBOOT_BOOTLOADER = 1,
  REBOOT_RECOVERY = 2,
};

void power_reboot(power_reboot_flags flags);
void power_shutdown();
uint32_t power_cpu_off();
uint32_t power_cpu_on(uint64_t mpid, paddr_t entry);

#endif  // ZIRCON_KERNEL_INCLUDE_DEV_POWER_H_
