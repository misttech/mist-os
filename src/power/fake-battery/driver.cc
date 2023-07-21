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

namespace fake_battery {

Driver::Driver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("fake-battery", std::move(start_args), std::move(driver_dispatcher)),
      devfs_connector_(fit::bind_member<&Driver::Serve>(this)) {}

zx::result<> Driver::Start() {
  node_.Bind(std::move(node()));
  auto result = AddChild(name());
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child node", KV("status", result.status_string()));
    return result.take_error();
  }
  return zx::ok();
}

zx::result<> Driver::AddChild(std::string_view node_name) {
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

void Driver::Serve(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server) {
  auto server_impl = std::make_unique<PowerSourceProtocolServer>();
  fidl::BindServer(dispatcher(), std::move(server), std::move(server_impl));
}

FUCHSIA_DRIVER_EXPORT(fake_battery::Driver);

}  // namespace fake_battery
