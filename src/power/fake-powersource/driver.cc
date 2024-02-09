// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver.h"

#include <fidl/fuchsia.hardware.powersource/cpp/natural_types.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fit/function.h>

#include <utility>

#include "power_source_state.h"

namespace fake_powersource {

Driver::Driver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("fake-powersource", std::move(start_args), std::move(driver_dispatcher)),
      devfs_connector_source_battery_(fit::bind_member<&Driver::ServeBattery>(this)),
      devfs_connector_sim_battery_(fit::bind_member<&Driver::ServeSimulatorBattery>(this)),
      protocol_server_battery_(fake_data_battery_),
      simulator_server_battery_(fake_data_battery_),
      devfs_connector_source_ac_(fit::bind_member<&Driver::ServeAc>(this)),
      devfs_connector_sim_ac_(fit::bind_member<&Driver::ServeSimulatorAc>(this)),
      protocol_server_ac_(fake_data_ac_),
      simulator_server_ac_(fake_data_ac_) {}

zx::result<> Driver::Start() {
  node_.Bind(std::move(node()));
  auto result = AddDriverAndControl("fake-battery", "battery-source-simulator",
                                    devfs_connector_source_battery_, devfs_connector_sim_battery_);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add fake battery driver and its control",
             KV("status", result.status_string()));
    return result.take_error();
  }

  result = AddDriverAndControl("fake-ac", "ac-source-simulator", devfs_connector_source_ac_,
                               devfs_connector_sim_ac_);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add fake ac driver and its control",
             KV("status", result.status_string()));
    return result.take_error();
  }

  return zx::ok();
}

template <typename A, typename B>
zx::result<> Driver::AddDriverAndControl(std::string_view driver_node_name,
                                         std::string_view sim_node_name,
                                         driver_devfs::Connector<A>& driver_devfs_connector,
                                         driver_devfs::Connector<B>& sim_devfs_connector) {
  auto result = AddChild(driver_node_name, "power", driver_devfs_connector);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child node fake battery", KV("status", result.status_string()));
    return result.take_error();
  }

  // (TODO:https://fxbug.dev/42081644) To allow the power-simulator to be found in
  // /dev/class/power-simulator, a line needs to be added to src/lib/ddk/include/lib/ddk/protodefs.h
  result = AddChild(sim_node_name, "power-sim", sim_devfs_connector);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child node battery simulator",
             KV("status", result.status_string()));
    return result.take_error();
  }

  return zx::ok();
}

// Add a child device node and offer the service capabilities.
template <typename Protocol>
zx::result<> Driver::AddChild(std::string_view node_name, std::string_view class_name,
                              driver_devfs::Connector<Protocol>& devfs_connector) {
  fidl::Arena arena;
  zx::result connector = devfs_connector.Bind(dispatcher());

  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name(class_name);

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
  controllers_.emplace_back(std::move(endpoints->client));
  return zx::ok();
}

void Driver::ServeBattery(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server) {
  protocol_server_battery_.Serve(dispatcher(), std::move(server));
}

void Driver::ServeSimulatorBattery(
    fidl::ServerEnd<fuchsia_hardware_powersource_test::SourceSimulator> server) {
  simulator_server_battery_.Serve(dispatcher(), std::move(server));
}

void Driver::ServeAc(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server) {
  protocol_server_ac_.Serve(dispatcher(), std::move(server));
}

void Driver::ServeSimulatorAc(
    fidl::ServerEnd<fuchsia_hardware_powersource_test::SourceSimulator> server) {
  simulator_server_ac_.Serve(dispatcher(), std::move(server));
}

}  // namespace fake_powersource

FUCHSIA_DRIVER_EXPORT(fake_powersource::Driver);
