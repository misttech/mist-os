// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_FUCHSIA_POWER_MANAGER_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_FUCHSIA_POWER_MANAGER_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/inspect/cpp/inspect.h>

#include "parent_device.h"

class FuchsiaPowerManager {
 public:
  class Owner {
   public:
    using PowerStateCallback = fit::callback<void(bool)>;
    virtual void SetPowerState(bool enabled, PowerStateCallback completer) = 0;
  };
  explicit FuchsiaPowerManager(Owner* owner);

  bool Initialize(ParentDevice* parent_device, inspect::Node& node);
  zx_status_t AcquireLease(
      const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client,
      fidl::ClientEnd<fuchsia_power_broker::LeaseControl>& lease_control_client_end);

  static constexpr char kHardwarePowerElementName[] = "mali-gpu-hardware";
  static constexpr uint8_t kPoweredDownPowerLevel = 0;
  static constexpr uint8_t kPoweredUpPowerLevel = 1;

 private:
  void CheckRequiredLevel();
  Owner* owner_;
  fidl::WireSyncClient<fuchsia_power_broker::Lessor> hardware_power_lessor_client_;
  fidl::WireClient<fuchsia_power_broker::LeaseControl> hardware_power_lease_control_client_;
  fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel> hardware_power_current_level_client_;
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> hardware_power_element_control_client_end_;
  fidl::WireClient<fuchsia_power_broker::RequiredLevel> hardware_power_required_level_client_;
  std::vector<zx::event> active_power_dep_tokens_;
  std::vector<zx::event> passive_power_dep_tokens_;

  inspect::BoolProperty power_lease_active_;
  inspect::UintProperty required_power_level_;
  inspect::UintProperty current_power_level_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_FUCHSIA_POWER_MANAGER_H_
