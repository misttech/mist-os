// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/power-state.h"

#include <lib/power-management/energy-model.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <atomic>
#include <optional>

#include <fbl/ref_ptr.h>

namespace power_management {

fbl::RefPtr<PowerDomain> PowerState::SetOrUpdateDomain(fbl::RefPtr<PowerDomain> domain) {
  if (!domain || (domain_ && domain->id() != domain_->id())) {
    idle_power_level_ = std::nullopt;
    active_power_level_ = std::nullopt;
    desired_active_power_level_ = std::nullopt;
  }
  domain_.swap(domain);
  return domain;
}

std::optional<PowerLevelUpdateRequest> PowerState::RequestTransition(uint32_t cpu_num,
                                                                     uint8_t power_level) {
  if (!domain_) {
    return std::nullopt;
  }

  if (!active_power_level_) {
    return std::nullopt;
  }

  if (power_level >= domain_->model().levels().size()) {
    return std::nullopt;
  }

  desired_active_power_level_ = power_level;
  if (desired_active_power_level_ == active_power_level_) {
    return std::nullopt;
  }

  auto level = domain_->model().levels()[power_level];

  PowerLevelUpdateRequest transition = {
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

zx::result<> PowerState::UpdatePowerLevel(ControlInterface control, uint64_t control_argument) {
  // While this transition was being executed, there was a model change, so we discard the
  // transition.
  if (domain_) {
    if (auto level = domain_->model().FindPowerLevel(control, control_argument)) {
      return UpdatePowerLevel(*level);
    }
    // No power level described by that tuple.
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  // There is no domain to request the change from.
  return zx::error(ZX_ERR_BAD_STATE);
}

zx::result<> PowerState::UpdatePowerLevel(size_t level) {
  if (level >= domain_->model().levels().size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  if (level >= domain_->model().idle_levels().size()) {
    active_power_level_ = level;
  } else {
    idle_power_level_ = level;
  }

  return zx::ok();
}

}  // namespace power_management
