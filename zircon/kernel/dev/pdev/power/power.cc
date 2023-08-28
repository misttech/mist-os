// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/intrin.h>

#include <dev/power.h>
#include <pdev/power.h>

static const struct pdev_power_ops default_ops = {
    .reboot = [](power_reboot_flags flags) {},
    .shutdown = []() {},
    .cpu_off = []() -> uint32_t { return 0; },
    .cpu_on = [](uint64_t mpid, paddr_t entry) -> uint32_t { return 0; },
    .get_cpu_state = [](uint64_t hw_cpu_id) -> zx::result<power_cpu_state> {
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }};

static const struct pdev_power_ops* power_ops = &default_ops;

void power_reboot(power_reboot_flags flags) { power_ops->reboot(flags); }
void power_shutdown() { power_ops->shutdown(); }
uint32_t power_cpu_off() { return power_ops->cpu_off(); }
uint32_t power_cpu_on(uint64_t hw_cpu_id, paddr_t entry) {
  return power_ops->cpu_on(hw_cpu_id, entry);
}
zx::result<power_cpu_state> power_get_cpu_state(uint64_t hw_cpu_id) {
  return power_ops->get_cpu_state(hw_cpu_id);
}

void pdev_register_power(const struct pdev_power_ops* ops) {
  power_ops = ops;
  arch::ThreadMemoryBarrier();
}
