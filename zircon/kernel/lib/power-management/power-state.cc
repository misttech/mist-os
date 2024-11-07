// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/power-state.h"

#include <lib/power-management/energy-model.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <optional>

#include <fbl/ref_ptr.h>

namespace power_management {

fbl::RefPtr<PowerDomain> PowerState::SetOrUpdateDomain(fbl::RefPtr<PowerDomain> domain) {
  if (domain_) {
    domain_->total_normalized_utilization_.fetch_sub(normalized_utilization_,
                                                     std::memory_order_relaxed);
  }

  if (!domain || (domain_ && domain->id() != domain_->id())) {
    active_power_level_ = std::nullopt;
    desired_active_power_level_ = std::nullopt;
  }

  if (domain) {
    domain->total_normalized_utilization_.fetch_add(normalized_utilization_,
                                                    std::memory_order_relaxed);
  }

  domain_.swap(domain);
  return domain;
}

std::optional<PowerLevelUpdateRequest> PowerState::RequestTransition(uint32_t cpu_num,
                                                                     uint8_t active_power_level) {
  if (!domain_) {
    return std::nullopt;
  }

  if (!active_power_level_) {
    return std::nullopt;
  }

  ZX_ASSERT(active_power_level >= domain_->model().idle_levels().size() &&
            active_power_level < domain_->model().levels().size());

  desired_active_power_level_ = active_power_level;
  if (desired_active_power_level_ == active_power_level_) {
    return std::nullopt;
  }

  const auto& level = domain_->model().levels()[active_power_level];

  PowerLevelUpdateRequest transition = {
      .domain_id = domain_->id(),
      .target_id = domain_->id(),
      .control = level.control(),
      .control_argument = level.control_argument(),
      .options = 0,
  };

  if (level.TargetsCpus()) {
    transition.target_id = cpu_num;
    transition.options |= ZX_PROCESSOR_POWER_LEVEL_OPTIONS_DOMAIN_INDEPENDENT;
  }

  return transition;
}

zx::result<> PowerState::UpdateActivePowerLevel(uint8_t level) {
  if (!domain_) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  if (level < domain_->model().idle_levels().size() || level >= domain_->model().levels().size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  active_power_level_ = level;
  return zx::ok();
}

}  // namespace power_management
