// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_FUCHSIA_POWER_MANAGER_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_FUCHSIA_POWER_MANAGER_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/inspect/cpp/inspect.h>

#include "parent_device.h"
#include "power_manager.h"
#include "timeout_source.h"

class FuchsiaPowerManager final : public TimeoutSource {
 public:
  class Owner {
   public:
    using PowerStateCallback = fit::callback<void(bool)>;
    virtual void PostPowerStateChange(bool enabled, PowerStateCallback completer) = 0;
    virtual PowerManager* GetPowerManager() = 0;
  };
  struct PowerGoals {
    zx::event on_ready_for_work_token;
  };

  class HardwareElementRunner : public fidl::Server<fuchsia_power_broker::ElementRunner> {
   public:
    explicit HardwareElementRunner(FuchsiaPowerManager& p) : parent_(p) {}
    void SetLevel(fuchsia_power_broker::ElementRunnerSetLevelRequest& request,
                  SetLevelCompleter::Sync& completer) override;
    void handle_unknown_method(
        fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementRunner> metadata,
        fidl::UnknownMethodCompleter::Sync& completer) override;

   private:
    FuchsiaPowerManager& parent_;
  };

  class OnReadyForWorkElementRunner : public fidl::Server<fuchsia_power_broker::ElementRunner> {
   public:
    void SetLevel(fuchsia_power_broker::ElementRunnerSetLevelRequest& request,
                  SetLevelCompleter::Sync& completer) override;
    void handle_unknown_method(
        fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementRunner> metadata,
        fidl::UnknownMethodCompleter::Sync& completer) override;
  };

  explicit FuchsiaPowerManager(Owner* owner);

  bool Initialize(ParentDevice* parent_device, inspect::Node& node);

  TimeoutSource::Clock::time_point GetCurrentTimeoutPoint() override;
  void TimeoutTriggered() override {
    DisablePower();
    MAGMA_DASSERT(!LeaseIsRequested());
  }

  bool EnablePower();
  bool DisablePower();
  bool LeaseIsRequested();

  PowerGoals GetPowerGoals();

  static constexpr char kHardwarePowerElementName[] = "mali-gpu-hardware";
  static constexpr uint8_t kPoweredDownPowerLevel = 0;
  static constexpr uint8_t kPoweredUpPowerLevel = 1;
  static constexpr char kOnReadyForWorkPowerElementName[] = "mali-on-ready-for-work";

 private:
  void CheckRequiredLevel();
  zx_status_t AcquireLease(
      const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client,
      fidl::ClientEnd<fuchsia_power_broker::LeaseControl>& lease_control_client_end);
  Owner* owner_;
  fidl::WireSyncClient<fuchsia_power_broker::Lessor> hardware_power_lessor_client_;

  fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel> hardware_power_current_level_client_;
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> hardware_power_element_control_client_end_;
  HardwareElementRunner hardware_power_element_runner_server_;
  std::optional<fidl::ServerBindingRef<fuchsia_power_broker::ElementRunner>>
      hardware_power_element_runner_server_bindref_;
  std::vector<zx::event> assertive_power_dep_tokens_;
  std::vector<zx::event> opportunistic_power_dep_tokens_;

  zx::event on_ready_for_work_token_;
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> on_ready_for_work_control_client_end_;
  OnReadyForWorkElementRunner on_ready_for_work_element_runner_server_;
  std::optional<fidl::ServerBindingRef<fuchsia_power_broker::ElementRunner>>
      on_ready_for_work_element_runner_server_bindref_;

  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control_client_end_;

  inspect::BoolProperty power_lease_active_;
  inspect::UintProperty required_power_level_;
  inspect::UintProperty current_power_level_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_FUCHSIA_POWER_MANAGER_H_
