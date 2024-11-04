// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/energy-model.h"

#include <lib/stdcompat/utility.h>
#include <zircon/errors.h>
#include <zircon/syscalls-next.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <optional>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>

namespace power_management {

namespace {

template <size_t N>
static constexpr bool HasOverlappingCpu(const uint64_t (&a)[N], const uint64_t (&b)[N]) {
  for (size_t i = 0; i < N; ++i) {
    if ((a[i] & b[i]) != 0) {
      return true;
    }
  }
  return false;
}

}  // namespace

zx::result<EnergyModel> EnergyModel::Create(
    cpp20::span<const zx_processor_power_level_t> levels,
    cpp20::span<const zx_processor_power_level_transition_t> transitions) {
  // Allocations below would be UB.
  if (levels.size() < 1) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (transitions.size() > levels.size() * levels.size()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Validate that transitions are to and from valid levels.
  for (const auto& transition : transitions) {
    if (transition.from >= levels.size()) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    if (transition.to >= levels.size()) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  // Validate that power level interfaces are supported values.
  for (const auto& level : levels) {
    if (!IsSupportedControlInterface(level.control_interface)) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  fbl::AllocChecker ac;
  fbl::Vector<PowerLevel> power_levels;
  power_levels.reserve(levels.size(), &ac);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  fbl::Vector<size_t> power_levels_lookup;
  power_levels_lookup.reserve(levels.size(), &ac);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  // If we assume symmetry in the matrix, we could reduce the amount of space required to support
  // transitions by more than half, since any element below the main diagonal would be equivalent to
  // its mirror, and the diagonal itself would be 0.
  fbl::Vector<PowerLevelTransition> power_level_transitions;
  PowerLevelTransition default_value =
      transitions.empty() ? PowerLevelTransition::Zero() : PowerLevelTransition::Invalid();

  power_level_transitions.resize(levels.size() * levels.size(), default_value, &ac);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  size_t idle_levels = 0;
  // We assert below, because all the space required for these operation has been preallocated.
  for (size_t i = 0; i < levels.size(); ++i) {
    power_levels.push_back(PowerLevel(static_cast<uint8_t>(i), levels[i]), &ac);
    // These were preallocated above.
    ZX_ASSERT(ac.check());
    power_levels_lookup.push_back(i, &ac);
    // These were preallocated above.
    ZX_ASSERT(ac.check());
    if (power_levels[i].type() == PowerLevel::kIdle) {
      idle_levels++;
    }
  }

  // Generate lookup table based on original indexes.
  std::sort(power_levels_lookup.begin(), power_levels_lookup.end(),
            [&power_levels](const size_t& a, const size_t& b) constexpr {
              if (power_levels[a].processing_rate() == power_levels[b].processing_rate()) {
                return power_levels[a].power_coefficient_nw() <
                       power_levels[b].power_coefficient_nw();
              }
              return power_levels[a].processing_rate() < power_levels[b].processing_rate();
            });

  // This will naturally partition idle and active states, having all idle states at the beginning.
  std::sort(power_levels.begin(), power_levels.end(),
            [](const PowerLevel& a, const PowerLevel& b) constexpr {
              if (a.processing_rate() == b.processing_rate()) {
                return a.power_coefficient_nw() < b.power_coefficient_nw();
              }
              return a.processing_rate() < b.processing_rate();
            });

  // Fill up the square matrix from level i to level j, where i and j, are indexes into the
  // power_level array.
  for (const auto& transition : transitions) {
    size_t i = power_levels_lookup[transition.from];
    size_t j = power_levels_lookup[transition.to];

    // Double check that translation is correct.
    ZX_ASSERT(power_levels[i].level() == transition.from);
    ZX_ASSERT(power_levels[j].level() == transition.to);

    size_t transition_offset = i * power_levels.size() + j;
    power_level_transitions[transition_offset] = PowerLevelTransition(transition);
  }

  // Reset the power level lookup, and turn it into a lookup by control interface, such that the set
  // path doesnt have to traverse every level.
  for (size_t i = 0; i < power_levels_lookup.size(); ++i) {
    power_levels_lookup[i] = i;
  }

  // Generate lookup table based on control interface and control argument tuple..
  std::sort(power_levels_lookup.begin(), power_levels_lookup.end(),
            [&power_levels](size_t a, size_t b) constexpr {
              if (cpp23::to_underlying(power_levels[a].control()) ==
                  cpp23::to_underlying(power_levels[b].control())) {
                return power_levels[a].control_argument() < power_levels[b].control_argument();
              }

              return cpp23::to_underlying(power_levels[a].control()) <
                     cpp23::to_underlying(power_levels[b].control());
            });

  return zx::ok(EnergyModel{std::move(power_levels), std::move(power_level_transitions),
                            std::move(power_levels_lookup), idle_levels});
}

zx::result<> PowerDomainRegistry::UpdateRegistry(
    fbl::RefPtr<PowerDomain> power_domain,
    fit::inline_function<void(size_t, fbl::RefPtr<PowerDomain>)> update_cpu_power_domain) {
  std::optional<decltype(domains_)::iterator> old_domain_prev;
  std::optional<decltype(domains_)::iterator> prev_it = std::nullopt;
  bool existing_id = false;
  for (auto it = domains_.begin(); it != domains_.end(); ++it) {
    auto& entry = *it;
    if (entry.id() == power_domain->id()) {
      old_domain_prev = prev_it;
      existing_id = true;
    } else if (HasOverlappingCpu(entry.cpus().mask, power_domain->cpus().mask)) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    prev_it = it;
  }

  fbl::RefPtr<PowerDomain> old_domain = nullptr;
  // Now remove old_domain from the list, and update the domain's generation number.
  if (existing_id) {
    if (!old_domain_prev) {
      old_domain = domains_.pop_front();
    } else {
      old_domain = domains_.erase_next(*old_domain_prev);
    }
  }

  // Update every CPU reference from previous power domain to `power_domain`.
  for (size_t i = 0; i < kBuckets; ++i) {
    const size_t bucket_offset = i * kBitsPerBucket;
    if (power_domain->cpus().mask[i] == 0 && (old_domain && old_domain->cpus().mask[i] == 0)) {
      continue;
    }

    for (size_t j = 0; j < kBitsPerBucket; ++j) {
      const uint64_t bit_mask = 1ull << j;
      const size_t num_cpu = bucket_offset + j;
      if ((power_domain->cpus().mask[i] & bit_mask) != 0) {
        // This would be done, for example under the scheduler`s `queue_lock_`, and we want to
        // keep it as short as possible.
        update_cpu_power_domain(num_cpu, power_domain);
      } else if (old_domain && (old_domain->cpus().mask[i] & bit_mask) != 0) {
        update_cpu_power_domain(num_cpu, nullptr);
      }
    }
  }

  domains_.push_front(std::move(power_domain));
  return zx::ok();
}

zx::result<> PowerDomainRegistry::RemoveFromRegistry(
    uint32_t domain_id, fit::inline_function<void(size_t)> clear_domain) {
  std::optional<decltype(domains_)::iterator> power_domain_prev;
  std::optional<decltype(domains_)::iterator> prev_it = std::nullopt;

  fbl::RefPtr<PowerDomain> power_domain = nullptr;
  bool existing_id = false;
  for (auto it = domains_.begin(); it != domains_.end(); ++it) {
    auto& entry = *it;
    if (entry.id() == domain_id) {
      power_domain_prev = prev_it;
      existing_id = true;
      break;
    }
    prev_it = it;
  }

  // Now remove power_domain from the list, and update the domain's generation number.
  if (existing_id) {
    if (!power_domain_prev) {
      power_domain = domains_.pop_front();
    } else {
      power_domain = domains_.erase_next(*power_domain_prev);
    }
  } else {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  for (size_t i = 0; i < kBuckets; ++i) {
    const size_t bucket_offset = i * kBitsPerBucket;
    for (size_t j = 0; j < kBitsPerBucket; ++j) {
      const uint64_t bit_mask = 1ull << j;
      const size_t num_cpu = bucket_offset + j;
      if ((power_domain->cpus().mask[i] & bit_mask) != 0) {
        clear_domain(num_cpu);
      }
    }
  }

  return zx::ok();
}

std::optional<uint8_t> EnergyModel::FindPowerLevel(ControlInterface interface_id,
                                                   uint64_t control_argument) const {
  auto it = std::lower_bound(control_lookup_.begin(), control_lookup_.end(),
                             zx_processor_power_level_t{
                                 .control_interface = cpp23::to_underlying(interface_id),
                                 .control_argument = control_argument,
                             },
                             [this](size_t i, const zx_processor_power_level_t& val) {
                               const auto& a = power_levels_[i];
                               return cpp23::to_underlying(a.control()) < val.control_interface ||
                                      (cpp23::to_underlying(a.control()) == val.control_interface &&
                                       a.control_argument() < val.control_argument);
                             });
  if (it != control_lookup_.end() && power_levels_[*it].control() == interface_id &&
      power_levels_[*it].control_argument() == control_argument) {
    return *it;
  }

  return std::nullopt;
}

}  // namespace power_management
