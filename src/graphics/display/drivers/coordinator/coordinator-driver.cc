// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/coordinator-driver.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display_coordinator {

CoordinatorDriver::CoordinatorDriver(fdf::DriverStartArgs start_args,
                                     fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("display-coordinator", std::move(start_args), std::move(driver_dispatcher)),
      devfs_connector_(fit::bind_member<&CoordinatorDriver::ConnectProvider>(this)) {}

CoordinatorDriver::~CoordinatorDriver() = default;

zx::result<> CoordinatorDriver::Start() {
  auto create_engine_driver_client_result = EngineDriverClient::Create(incoming());
  if (create_engine_driver_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create EngineDriverClient: %s",
            create_engine_driver_client_result.status_string());
    return create_engine_driver_client_result.take_error();
  }

  const char kSchedulerRoleName[] = "fuchsia.graphics.display.drivers.display.controller";
  zx::result<fdf::SynchronizedDispatcher> create_client_dispatcher_result =
      fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "display-client-loop",
          [](fdf_dispatcher_t* dispatcher) {
            FDF_LOG(DEBUG, "Display coordinator client dispatcher is shut down.");
          },
          kSchedulerRoleName);
  if (create_client_dispatcher_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create dispatcher: %s",
            create_client_dispatcher_result.status_string());
    return create_client_dispatcher_result.take_error();
  }
  client_dispatcher_ = std::move(create_client_dispatcher_result).value();

  zx::result<std::unique_ptr<Controller>> create_controller_result = Controller::Create(
      std::move(create_engine_driver_client_result).value(), client_dispatcher_.borrow());
  if (create_controller_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create Controller: %s", create_controller_result.status_string());
    return create_controller_result.take_error();
  }

  controller_ = std::move(create_controller_result).value();
  InitInspectorExactlyOnce(controller_->inspector());

  // Create a node for devfs.
  zx::result<fidl::ClientEnd<fuchsia_device_fs::Connector>> bind_devfs_connector_result =
      devfs_connector_.Bind(dispatcher());
  if (bind_devfs_connector_result.is_error()) {
    FDF_LOG(ERROR, "Failed to bind to devfs connector: %s",
            bind_devfs_connector_result.status_string());
    return bind_devfs_connector_result.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs{{
      .connector = std::move(bind_devfs_connector_result).value(),
      .class_name = "display-coordinator",
      .connector_supports = fuchsia_device_fs::ConnectionType::kDevice,
  }};

  zx::result<fdf::OwnedChildNode> add_child_result = AddOwnedChild(name(), devfs);
  if (add_child_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add child node: %s", add_child_result.status_string());
    return add_child_result.take_error();
  }

  auto [controller_client, node_client] = std::move(add_child_result).value();
  node_controller_.Bind(std::move(controller_client));
  node_.Bind(std::move(node_client));
  return zx::ok();
}

void CoordinatorDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  controller_->PrepareStop();
  completer(zx::ok());
}

void CoordinatorDriver::Stop() { controller_->Stop(); }

void CoordinatorDriver::ConnectProvider(
    fidl::ServerEnd<fuchsia_hardware_display::Provider> provider_request) {
  provider_bindings_.AddBinding(dispatcher(), std::move(provider_request), controller_.get(),
                                fidl::kIgnoreBindingClosure);
}

}  // namespace display_coordinator

FUCHSIA_DRIVER_EXPORT(display_coordinator::CoordinatorDriver);
