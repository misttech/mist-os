// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_POWER_STATE_H_
#define ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_POWER_STATE_H_

#include <stdint.h>
#include <zircon/errors.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/port.h>

#include <atomic>
#include <cstdint>
#include <optional>

#include <fbl/intrusive_single_list.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

#include "energy-model.h"

namespace power_management {

// Represents a requested power level update to be executed at a later stage.
struct PowerLevelUpdateRequest {
  constexpr zx_port_packet port_packet() const {
    return {.key = domain_id,
            .type = ZX_PKT_TYPE_PROCESSOR_POWER_LEVEL_TRANSITION_REQUEST,
            .status = ZX_OK,
            .processor_power_level_transition = {
                .domain_id = target_id,
                .options = options,
                .control_interface = static_cast<uint64_t>(control),
                .control_argument = control_argument,
            }};
  }

  // Domain ID where the request is originating from.
  uint32_t domain_id;

  // Target ID of the device or domain that should be transitioned.
  uint32_t target_id;

  // Interface this request should be routed to.
  ControlInterface control;

  // Argument used for transitioning through the control interface.
  uint64_t control_argument;

  // Options that determine what `target_id` is. This is determined by the
  // power level being transitioned to.
  uint32_t options;
};

// PowerState encapsulates the current power level, power domain, and energy model for an individual
// processor.
//
// Instances of PowerState are not safe for concurrent use and must be protected by an external lock
// associated with the owning processor.
class PowerState {
 public:
  PowerState() = default;

  // Mostly useful for testing, for validating transitions.
  PowerState(fbl::RefPtr<PowerDomain> domain, std::optional<uint8_t> idle_power_level,
             std::optional<uint8_t> active_power_level,
             std::optional<uint8_t> desired_active_power_level)
      : domain_(std::move(domain)),
        idle_power_level_(idle_power_level),
        active_power_level_(active_power_level),
        desired_active_power_level_(desired_active_power_level) {}

  // Domain the PowerState is being modeled after.
  const fbl::RefPtr<PowerDomain>& domain() const { return domain_; }

  // Returns whether the domain's controller is serving requests. Returns false if there is no
  // domain.
  bool is_serving() const { return domain() && domain()->controller()->is_serving(); }

  // Active power level when device is idle.
  constexpr bool is_idle() const { return !!idle_power_level_; }
  constexpr std::optional<uint8_t> idle_power_level() const { return idle_power_level_; }

  // Active power level when device is not idle.
  constexpr bool is_active() const { return !is_idle() && !!active_power_level_; }
  constexpr std::optional<uint8_t> active_power_level() const { return active_power_level_; }

  // Returns the power level currently affecting the device's power state.
  constexpr std::optional<uint8_t> power_level() const {
    return is_idle() ? idle_power_level_ : active_power_level_;
  }

  // Power level the device needs to be transitioned to.
  constexpr std::optional<uint8_t> desired_active_power_level() const {
    return desired_active_power_level_;
  }

  // Sets the `PowerDomain` and related models that this `PowerState` references. This means,
  // that any `power_level` or `desired_power_level` is only meaningful for that specific
  // `PowerDomain`.
  fbl::RefPtr<PowerDomain> SetOrUpdateDomain(fbl::RefPtr<PowerDomain> domain);

  // Request transition this power state to a given power level, as described on `domain_->model()`.
  //
  // `cpu_num` represents the number of the device associated with this power state.
  // `level` represents the desired power level.
  //
  // A `PendingPowerLevelTransition` is provided when there is a change in the desired active power
  // level.
  std::optional<PowerLevelUpdateRequest> RequestTransition(uint32_t cpu_num, uint8_t level);

  // Attempts to update `PowerState::power_level_`. This only succeeds if the underlying model
  // matches the one referenced on `update`.
  zx::result<> UpdatePowerLevel(ControlInterface control, uint64_t control_argument);

  // Sets the underlying power level.
  zx::result<> UpdatePowerLevel(uint8_t level);

  // When a device transitions from an idle state into an active state, this is reflected as
  // clearing the idle power level.
  void TransitionFromIdle() { idle_power_level_ = std::nullopt; }

  // Update's the associated power domain's total utilization.
  void UpdateUtilization(int64_t utilization_delta) {
    domain_->total_normalized_utilization_.fetch_add(utilization_delta);
  }

 private:
  // Power domain the device belongs to, with the model it requires.
  fbl::RefPtr<PowerDomain> domain_ = nullptr;

  // When on an idle state, represents the power level currently active on the entity.
  std::optional<uint8_t> idle_power_level_ = std::nullopt;

  // Active power level of the entity, this is not affected by switching to and from idle power
  // states.
  std::optional<uint8_t> active_power_level_ = std::nullopt;

  // Represents the power level that needs be to transitioned to.
  std::optional<uint8_t> desired_active_power_level_ = std::nullopt;
};

}  // namespace power_management

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_POWER_STATE_H_
