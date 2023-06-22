// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake-battery-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include "fidl/fuchsia.hardware.powersource/cpp/natural_types.h"

using fuchsia_hardware_powersource::wire::BatteryInfo;
using fuchsia_hardware_powersource::wire::BatteryUnit;
using fuchsia_hardware_powersource::wire::PowerType;
using fuchsia_hardware_powersource::wire::SourceInfo;

namespace fake_battery {

zx_status_t PowerSourceProtocolServer::ClearSignal() {
  zx_status_t status = state_event_.signal(ZX_USER_SIGNAL_0, 0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to clear signal on event: %s", zx_status_get_string(status));
  }
  return status;
}

zx_status_t PowerSourceProtocolServer::SignalClient() {
  zx_status_t status = state_event_.signal(0, ZX_USER_SIGNAL_0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to set signal on event: %s", zx_status_get_string(status));
  }
  return status;
}

void PowerSourceProtocolServer::GetPowerInfo(GetPowerInfoCompleter::Sync& completer) {
  SourceInfo source_info{PowerType::kBattery,  // PowerType::kAc,
                         fuchsia_hardware_powersource::kPowerStateCharging |
                             fuchsia_hardware_powersource::kPowerStateOnline};
  completer.Reply(ZX_OK, source_info);
}

// TODO(bihai): There is no unit test on this function yet. We will depcreated this and soft
// transition to a new set of API.
void PowerSourceProtocolServer::GetStateChangeEvent(GetStateChangeEventCompleter::Sync& completer) {
  zx::event clone;
  zx::event::create(0, &state_event_);
  zx_status_t status = state_event_.duplicate(ZX_RIGHT_WAIT | ZX_RIGHT_TRANSFER, &clone);
  if (status == ZX_OK) {
    // Clear signal before returning.
    ClearSignal();
  }
  completer.Reply(status, std::move(clone));
}

void PowerSourceProtocolServer::GetBatteryInfo(GetBatteryInfoCompleter::Sync& completer) {
  BatteryInfo battery_info{
      .unit = BatteryUnit::kMa,
      .design_capacity = 3000,
      .last_full_capacity = 2950,
      .design_voltage = 3000,  // mV
      .capacity_warning = 800,
      .capacity_low = 500,
      .capacity_granularity_low_warning = 20,
      .capacity_granularity_warning_full = 1,
  };

  battery_info.present_rate = 2;
  battery_info.remaining_capacity = 45;
  battery_info.present_voltage = 2900;
  completer.Reply(ZX_OK, battery_info);
}

class Driver : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("fake-battery", std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&Driver::Serve>(this)) {}

  zx::result<> Start() override {
    node_.Bind(std::move(node()));
    auto result = AddChild(name());
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add child node", KV("status", result.status_string()));
      return result.take_error();
    }
    return zx::ok();
  }

  // Add a child device node and offer the service capabilities.
  zx::result<> AddChild(std::string_view node_name) {
    fidl::Arena arena;
    zx::result connector = devfs_connector_.Bind(dispatcher());

    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                     .connector(std::move(connector.value()))
                     .class_name("power");

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, node_name)
                    .devfs_args(devfs.Build())
                    .Build();

    // Create endpoints of the `NodeController` for the node.
    auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    if (endpoints.is_error()) {
      FDF_SLOG(ERROR, "Failed to create endpoint", KV("status", endpoints.status_string()));
      return zx::error(endpoints.status_value());
    }
    auto result = node_->AddChild(args, std::move(endpoints->server), {});

    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      return zx::error(result.status());
    }
    controller_.Bind(std::move(endpoints->client));

    return zx::ok();
  }

 private:
  // Start serving Protocol (to be called by the devfs connector when a connection is established).
  void Serve(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server) {
    auto server_impl = std::make_unique<PowerSourceProtocolServer>();
    fidl::BindServer(dispatcher(), std::move(server), std::move(server_impl));
  }

  driver_devfs::Connector<fuchsia_hardware_powersource::Source> devfs_connector_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

FUCHSIA_DRIVER_EXPORT(fake_battery::Driver);

}  // namespace fake_battery
