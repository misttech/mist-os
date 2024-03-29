// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake_suspend.h"

#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace fake_suspend {

Driver::Driver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("fake-suspend", std::move(start_args), std::move(driver_dispatcher)),
      suspender_connector_(fit::bind_member<&Driver::ServeSuspender>(this)),
      control_connector_(fit::bind_member<&Driver::ServeSuspendControl>(this)),
      suspend_states_(std::make_shared<std::vector<fuchsia_hardware_suspend::SuspendState>>()),
      device_server_(std::make_shared<DeviceServer>(suspend_states_)),
      control_server_(std::make_shared<ControlServer>(suspend_states_)) {
  device_server_->set_suspend_observer(control_server_);
  control_server_->set_resumable(device_server_);
}

void Driver::ServeSuspender(fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> server) {
  FDF_SLOG(INFO, "Serving Suspender device");
  device_server_->Serve(dispatcher(), std::move(server));
}

void Driver::ServeSuspendControl(fidl::ServerEnd<test_suspendcontrol::Device> server) {
  FDF_SLOG(INFO, "Serving Suspender control device");
  control_server_->Serve(dispatcher(), std::move(server));
}

zx::result<> Driver::Start() {
  FDF_SLOG(INFO, "Starting fake_suspend driver");
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::Node>*> device_result =
      AddChild(&node(), name(), "suspend", suspender_connector_);
  if (device_result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add device node");
    return device_result.take_error();
  }

  auto control_result = AddChild(device_result.value(), "control", "test", control_connector_);
  if (control_result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add control node");
    return control_result.take_error();
  }

  return zx::ok();
}

// Add a child device node and offer the service capabilities.
template <typename T>
zx::result<fidl::ClientEnd<fuchsia_driver_framework::Node>*> Driver::AddChild(
    fidl::ClientEnd<fuchsia_driver_framework::Node>* parent, std::string_view node_name,
    std::string_view class_name, driver_devfs::Connector<T>& devfs_connector) {
  fidl::Arena arena;
  zx::result connector = devfs_connector.Bind(dispatcher());

  if (connector.is_error()) {
    FDF_SLOG(ERROR, "Failed to bind connector", KV("status", node_name));
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .connector_supports(fuchsia_device_fs::ConnectionType::kDevice |
                                       fuchsia_device_fs::ConnectionType::kController)
                   .class_name(class_name);

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, node_name)
                  .devfs_args(devfs.Build())
                  .Build();

  // Create endpoints of the `NodeController` for the node.
  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_SLOG(ERROR, "Failed to create node controller endpoint",
             KV("status", controller_endpoints.status_string()));
    return zx::error(controller_endpoints.status_value());
  }

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (node_endpoints.is_error()) {
    FDF_SLOG(ERROR, "Failed to create node endpoint", KV("status", node_endpoints.status_string()));
    return zx::error(node_endpoints.status_value());
  }

  fidl::WireResult result = fidl::WireCall(*parent)->AddChild(
      args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
    return zx::error(result.status());
  }

  controllers_.emplace_back(std::move(controller_endpoints->client));
  nodes_.emplace_back(std::move(node_endpoints->client));

  return zx::ok(&nodes_.back());
}

}  // namespace fake_suspend

FUCHSIA_DRIVER_EXPORT(fake_suspend::Driver);
