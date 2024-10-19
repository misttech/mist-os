// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/kernel-registry.h"

#include <kernel/percpu.h>

namespace power_management {

PowerDomainRegistry KernelPowerDomainRegistry::registry_;

zx::result<> KernelPowerDomainRegistry::Register(fbl::RefPtr<PowerDomain> domain) {
  Guard<Mutex> guard(registry_lock_::Get());
  return registry_.Register(
      ktl::move(domain), [](size_t num_cpu, fbl::RefPtr<power_management::PowerDomain> domain) {
        // We should never see a `num_cpu` higher than `arch_num_cpus`, since we
        // validated the cpu mask earlier.
        ZX_ASSERT(num_cpu < arch_max_num_cpus());
        percpu::Get(static_cast<cpu_num_t>(num_cpu)).scheduler.SetPowerDomain(ktl::move(domain));
      });
}

zx::result<> KernelPowerDomainRegistry::Unregister(uint32_t domain_id) {
  Guard<Mutex> guard(registry_lock_::Get());
  return registry_.Unregister(domain_id, [](size_t num_cpu) {
    fbl::RefPtr<power_management::PowerDomain> empty = nullptr;
    percpu::Get(static_cast<cpu_num_t>(num_cpu)).scheduler.SetPowerDomain(ktl::move(empty));
  });
}

}  // namespace power_management
