// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/kernel-registry.h"

#include <zircon/errors.h>
#include <zircon/types.h>

#include <kernel/cpu.h>
#include <kernel/percpu.h>

namespace power_management {

PowerDomainRegistry KernelPowerDomainRegistry::registry_;

zx::result<> KernelPowerDomainRegistry::Register(fbl::RefPtr<PowerDomain> domain) {
  Guard<Mutex> guard(registry_lock_::Get());
  return registry_.Register(ktl::move(domain),
                            [](size_t num_cpu, fbl::RefPtr<power_management::PowerDomain> domain) {
                              // We should never see a `num_cpu` higher than `arch_num_cpus`, since
                              // we validated the cpu mask earlier.
                              ZX_ASSERT(num_cpu < arch_max_num_cpus());
                              percpu::Get(static_cast<cpu_num_t>(num_cpu))
                                  .scheduler.ExchangePowerDomain(ktl::move(domain));
                            });
}

zx::result<> KernelPowerDomainRegistry::Unregister(uint32_t domain_id) {
  Guard<Mutex> guard(registry_lock_::Get());
  return registry_.Unregister(domain_id, [](size_t num_cpu) {
    fbl::RefPtr<power_management::PowerDomain> empty = nullptr;
    percpu::Get(static_cast<cpu_num_t>(num_cpu)).scheduler.ExchangePowerDomain(ktl::move(empty));
  });
}

zx::result<> KernelPowerDomainRegistry::UpdateDomainPowerLevel(
    uint32_t domain_id, uint64_t controller_id, power_management::ControlInterface interface,
    uint64_t arg) {
  // This is a very broad serialization, we could get away with a R/W lock.
  //
  // The only writer lock is held while registering a power domain, since it may change the
  // relationship of existing power domains. All other uses would be a reader lock, which means
  // there would be no real serialization.
  //
  // Our goal here is to hold the scheduler lock for the least amount of time, and we can get away
  // with that, if we can guarantee that the domain is not getting updated/removed underneath us,
  // since we can perform the look up of the power level outside of the scheduler and just perform a
  // simple range check and set.
  Guard<Mutex> guard(registry_lock_::Get());
  auto domain = registry_.Find(domain_id);
  if (!domain) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  if (domain->controller()->id() != controller_id) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  auto power_level = domain->model().FindPowerLevel(interface, arg);
  if (!power_level) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  if (*power_level < domain->model().idle_levels().size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  for (size_t i = 0; i <= (arch_max_num_cpus() - 1) / ZX_CPU_SET_BITS_PER_WORD; ++i) {
    const cpu_num_t cpu_offset = static_cast<cpu_num_t>(i * ZX_CPU_SET_BITS_PER_WORD);
    const uint32_t max_bits =
        ktl::min(arch_max_num_cpus() - cpu_offset, static_cast<uint32_t>(ZX_CPU_SET_BITS_PER_WORD));
    for (size_t j = 0; j < max_bits; ++j) {
      if ((domain->cpus().mask[i] & 1ull << j) != 0) {
        const cpu_num_t cpu_num = static_cast<cpu_num_t>(j + cpu_offset);
        ZX_DEBUG_ASSERT(cpu_num < arch_max_num_cpus());

        zx::result result = percpu::Get(cpu_num).scheduler.SetPowerLevel(*power_level);
        ZX_ASSERT_MSG(result.is_ok(), "Unexpected error setting power level: %d",
                      result.status_value());
      }
    }
  }

  return zx::ok();
}

}  // namespace power_management
