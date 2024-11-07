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

  // Domain the PowerState is being modeled after.
  const fbl::RefPtr<PowerDomain>& domain() const { return domain_; }

  // Returns whether the domain's controller is serving requests. Returns false if there is no
  // domain.
  bool is_serving() const { return domain() && domain()->controller()->is_serving(); }

  // Returns the active power level when device is not idle.
  constexpr std::optional<uint8_t> active_power_level() const { return active_power_level_; }

  // Returns the active power level the device is in the processo of transitioning to.
  constexpr std::optional<uint8_t> desired_active_power_level() const {
    return desired_active_power_level_;
  }

  // Returns the maximum (i.e. highest power, lowest latency) idle power level.
  std::optional<uint8_t> max_idle_power_level() const {
    if (domain()) {
      return domain()->model().max_idle_power_level();
    }
    return std::nullopt;
  }

  // Returns the power coefficient of the current active power level. Returns zero if there is no
  // active power level or no domain is set.
  uint64_t active_power_coefficient_nw() const {
    if (domain() && active_power_level_.has_value()) {
      return domain()->model().levels()[*active_power_level_].power_coefficient_nw();
    }
    return 0;
  }

  // Returns the power coefficient of the maximum idle power level. Returns zero if there is no
  // idle power level or no domain is set.
  uint64_t max_idle_power_coefficient_nw() const {
    if (domain()) {
      return domain()->model().max_idle_power_coefficient_nw().value_or(0);
    }
    return 0;
  }

  // Returns the control interface of the maximum idle power level. Returns nullopt if there is no
  // idle power level or no domain is set.
  std::optional<ControlInterface> max_idle_power_level_interface() const {
    if (domain()) {
      return domain()->model().max_idle_power_level_interface();
    }
    return std::nullopt;
  }

  // Returns the current utilization of the processor.
  uint64_t normalized_utilization() const { return normalized_utilization_; }

  // Sets the `PowerDomain` and related models that this `PowerState` references. This means,
  // that any `power_level` or `desired_power_level` is only meaningful for that specific
  // `PowerDomain`.
  fbl::RefPtr<PowerDomain> SetOrUpdateDomain(fbl::RefPtr<PowerDomain> domain);

  // Posts a request to transition the power domain associated with this power state to a given
  // active power level from the current energy model.
  //
  // Ignored when no domain is set. Asserts that the requested power level is an active power level.
  //
  // `cpu_num` is the CPU associated with this power state.
  // `level` is the desired power level.
  //
  std::optional<PowerLevelUpdateRequest> RequestTransition(uint32_t cpu_num,
                                                           uint8_t active_power_level);

  // Updates the active power level of this power state. Typically called in response to the power
  // level controller completing a power level transition request.
  zx::result<> UpdateActivePowerLevel(uint8_t active_power_level);

  // Update the utilization of this processor and the total utilization of its domain.
  void UpdateUtilization(int64_t utilization_delta) {
    normalized_utilization_ += utilization_delta;
    if (domain()) {
      domain()->total_normalized_utilization_.fetch_add(utilization_delta,
                                                        std::memory_order_acq_rel);
    }
  }

 private:
  // The power domain the processor belongs to.
  fbl::RefPtr<PowerDomain> domain_;

  // The power level when the processor is actively running.
  std::optional<uint8_t> active_power_level_;

  // Represents the power level that needs be to transitioned to.
  std::optional<uint8_t> desired_active_power_level_;

  // The current normalized utilization of the processor.
  uint64_t normalized_utilization_{0};
};

}  // namespace power_management

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_POWER_STATE_H_
