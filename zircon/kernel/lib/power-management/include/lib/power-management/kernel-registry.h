// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_KERNEL_REGISTRY_H_
#define ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_KERNEL_REGISTRY_H_

#include <lib/power-management/energy-model.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>

namespace power_management {

// Kernel wrapper for referencing the registry and providing external synchronization.
class KernelPowerDomainRegistry {
  // Initialization happens early on, when we register the default power model.
  DECLARE_SINGLETON_MUTEX(registry_lock_);

 public:
  // Acquires proper lock and register `domain` with the underlying `PowerDomainRegistry` and
  // set the respective CPU's scheduler power state to hold a reference to `domain`.
  static zx::result<> Register(fbl::RefPtr<PowerDomain> domain) TA_EXCL(registry_lock_::Get());

  // Unregisters a domain matching `domain_id` and removes any references to the domain from
  // the associated scheduler's power state.
  static zx::result<> Unregister(uint32_t domain_id) TA_EXCL(registry_lock_::Get());

  // Update power level for a power domain.
  static zx::result<> UpdateDomainPowerLevel(uint32_t domain_id, uint64_t controller_id,
                                             power_management::ControlInterface interface,
                                             uint64_t arg);

  template <typename V>
  static void Visit(V&& v) {
    Guard<Mutex> guard(registry_lock_::Get());
    registry_.Visit(ktl::forward<V>(v));
  }

 private:
  static PowerDomainRegistry registry_ TA_GUARDED(registry_lock_::Get());
};

}  // namespace power_management

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_KERNEL_REGISTRY_H_
