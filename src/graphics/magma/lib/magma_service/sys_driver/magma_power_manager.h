// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_SYS_DRIVER_MAGMA_POWER_MANAGER_H_
#define SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_SYS_DRIVER_MAGMA_POWER_MANAGER_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/inspect/cpp/inspect.h>

namespace msd {

// This manages a power element. It receives FIDL calls from the power framework
// to change the power level. It is also able to manually drive power changes
// by creating and destroying a lease on the power element.
class PowerElementRunner : public fidl::Server<fuchsia_power_broker::ElementRunner> {
 public:
  // This is the Owner of this power element, it will receive callbacks when the
  // power frameworks wants to make a power change.
  class Owner {
   public:
    using PowerStateCallback = fit::callback<void(zx_status_t)>;
    virtual void PostPowerStateChange(int64_t power_state, PowerStateCallback completer) = 0;
  };

  explicit PowerElementRunner(fidl::WireSyncClient<fuchsia_power_broker::Lessor> lessor,
                              fidl::ServerEnd<fuchsia_power_broker::ElementRunner> element_server,
                              fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control,
                              Owner& owner)
      : owner_(owner),
        lessor_(std::move(lessor)),
        element_control_(std::move(element_control)),
        server_binding_(fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                         std::move(element_server), this)) {}

  static zx::result<std::unique_ptr<PowerElementRunner>> Create(
      fidl::ClientEnd<fuchsia_hardware_platform_device::Device>& pdev, fdf::Namespace& incoming,
      const char* pdev_element_name, PowerElementRunner::Owner& owner);

  void SetupInspect(inspect::Node& node);

  // Enable power by taking a lease on this element. This returns after the lease request
  // finishes, although that does not mean the power element has been fully enabled.
  zx_status_t EnablePower();

  // Disable power by dropping any held leases on this element.
  // If the power element is being kept on for other reasons it may continue to be enabled.
  // Returns ZX_ERR_BAD_STATE if we are not holding a lease.
  zx_status_t DisablePower();

  // fuchsia_power_broker::ElementRunner implementation.
  void SetLevel(fuchsia_power_broker::ElementRunnerSetLevelRequest& request,
                SetLevelCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementRunner> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  Owner& owner_;

  fidl::WireSyncClient<fuchsia_power_broker::Lessor> lessor_;
  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control_;
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control_;

  struct {
    inspect::BoolProperty power_lease_active;
    inspect::UintProperty required_power_level;
    inspect::UintProperty current_power_level;
  } inspect_;

  fidl::ServerBindingRef<fuchsia_power_broker::ElementRunner> server_binding_;
};

struct PowerInfo {
  fidl::WireSyncClient<fuchsia_power_broker::Lessor> lessor;
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control;
  fidl::ServerEnd<fuchsia_power_broker::ElementRunner> element_server;
};

zx::result<PowerInfo> CreatePowerInfo(
    fdf::Namespace& incoming, fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
    const std::vector<fdf_power::PowerElementConfiguration>& configs, const char* name);

}  // namespace msd

#endif  // SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_SYS_DRIVER_MAGMA_POWER_MANAGER_H_
