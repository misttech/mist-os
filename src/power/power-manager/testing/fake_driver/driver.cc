// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver.h"

#include <lib/driver/logging/cpp/structured_logger.h>

namespace fake_driver {

Driver::Driver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("fake-driver", std::move(start_args), std::move(driver_dispatcher)),
      devfs_connector_(fit::bind_member<&Driver::Serve>(this)),
      temperature_server_(&temperature_) {}

zx::result<> Driver::Start() {
  node_.Bind(std::move(node()));
  auto result = AddChild(name(), "temperature");
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child node", KV("status", result.status_string()));
    return result.take_error();
  }

  return zx::ok();
}

// Add a child device node and offer the service capabilities.
zx::result<> Driver::AddChild(std::string_view node_name, std::string_view class_name) {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());

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
  controller_.Bind(std::move(endpoints->client));
  return zx::ok();
}

void Driver::Serve(fidl::ServerEnd<fuchsia_hardware_temperature::Device> server) {
  temperature_server_.Serve(dispatcher(), std::move(server));
}

}  // namespace fake_driver

FUCHSIA_DRIVER_EXPORT(fake_driver::Driver);
