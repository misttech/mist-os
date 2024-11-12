// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_TEST_HELPER_H_
#define ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_TEST_HELPER_H_

#include <lib/power-management/energy-model.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>

#include <fbl/ref_ptr.h>

constexpr uint8_t kDomainIndependantPowerLevel = 0;
constexpr uint8_t kDefaultMaxPowerLevels = 10;
constexpr uint8_t kMaxIdlePowerLevel = 2;
constexpr uint8_t kMinActivePowerLevel = kMaxIdlePowerLevel + 1;

// Makes a power model of `power_levels` using the helpers below to determine
// the level properties. In many cases costs are defined in an arbitrary way based on
// the level indexes.
power_management::EnergyModel MakeFakeEnergyModel(size_t power_levels);

template <typename... Cpus>
zx_cpu_set_t MakeCpuSet(Cpus... cpus) {
  zx_cpu_set_t set = {};
  auto set_bit = [&set](size_t num_cpu) {
    set.mask[num_cpu / ZX_CPU_SET_BITS_PER_WORD] |= uint64_t(1)
                                                    << num_cpu % ZX_CPU_SET_BITS_PER_WORD;
    return true;
  };
  (set_bit(cpus) && ...);
  return set;
}

inline auto MakePowerDomain(uint32_t id, power_management::EnergyModel& model, zx_cpu_set_t cpus) {
  return fbl::MakeRefCounted<power_management::PowerDomain>(id, cpus, std::move(model));
}

template <typename... Cpus>
inline auto MakePowerDomainHelper(uint32_t id, Cpus... cpus) {
  auto model = MakeFakeEnergyModel(kDefaultMaxPowerLevels);
  return MakePowerDomain(id, model, MakeCpuSet(cpus...));
}

template <typename... Cpus>
inline auto MakePowerDomainHelper(uint32_t id, power_management::EnergyModel& model, Cpus... cpus) {
  return MakePowerDomain(id, model, MakeCpuSet(cpus...));
}

template <typename CpuVisitor>
void ForEachCpuIn(zx_cpu_set_t cpus, CpuVisitor&& visitor) {
  for (size_t i = 0; i < ZX_CPU_SET_MAX_CPUS / ZX_CPU_SET_BITS_PER_WORD; ++i) {
    if (cpus.mask[i] == 0) {
      continue;
    }
    for (size_t j = 0; j < ZX_CPU_SET_BITS_PER_WORD; ++j) {
      if ((cpus.mask[i] & (static_cast<uint64_t>(1) << j)) == 0) {
        continue;
      }
      visitor(i * ZX_CPU_SET_BITS_PER_WORD + j);
    }
  }
}

constexpr power_management::ControlInterface ControlInterfaceIdForLevel(size_t i) {
  // PSCI retention and powerdown levels.
  if (i < kMaxIdlePowerLevel) {
    return power_management::ControlInterface::kArmPsci;
  }

  // WFI power level. This idle state can also be entered via PSCI standby, which is redundant.
  if (i == kMaxIdlePowerLevel) {
    return power_management::ControlInterface::kArmWfi;
  }

  // Active power levels.
  return power_management::ControlInterface::kCpuDriver;
}

constexpr uint64_t ControlInterfaceArgForLevel(size_t i) { return i; }

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_TEST_HELPER_H_
