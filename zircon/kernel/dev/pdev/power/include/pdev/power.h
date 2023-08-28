// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_DEV_PDEV_POWER_INCLUDE_PDEV_POWER_H_
#define ZIRCON_KERNEL_DEV_PDEV_POWER_INCLUDE_PDEV_POWER_H_

#include <zircon/compiler.h>

#include <dev/power.h>

// power interface
struct pdev_power_ops {
  void (*reboot)(power_reboot_flags flags);
  void (*shutdown)();
  uint32_t (*cpu_off)();
  uint32_t (*cpu_on)(uint64_t hw_cpu_id, paddr_t entry);
  zx::result<power_cpu_state> (*get_cpu_state)(uint64_t hw_cpu_id);
};

void pdev_register_power(const pdev_power_ops* ops);

#endif  // ZIRCON_KERNEL_DEV_PDEV_POWER_INCLUDE_PDEV_POWER_H_
