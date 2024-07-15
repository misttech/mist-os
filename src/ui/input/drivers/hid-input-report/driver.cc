// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/input/drivers/hid-input-report/driver.h"

#include <lib/driver/component/cpp/driver_export.h>

namespace hid_input_report_dev {

namespace finput = fuchsia_hardware_input;

zx::result<> InputReportDriver::Start() {
  zx::result<fidl::ClientEnd<finput::Controller>> controller =
      incoming()->Connect<finput::Service::Controller>();
  if (controller.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to fuchsia_hardware_input service. %s",
            controller.status_string());
    return controller.take_error();
  }

  {
    auto [client, server] = fidl::Endpoints<finput::Device>::Create();
    auto result = fidl::WireCall(controller.value())->OpenSession(std::move(server));
    if (!result.ok()) {
      return zx::error(result.status());
    }

    input_report_ = std::make_unique<InputReport>(std::move(client));
  }

  // Expose the driver's inspect data.
  InitInspectorExactlyOnce(input_report_->Inspector());

  // Start the inner DFv1 driver.
  auto status = input_report_->Start();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to start input report %d", status);
    return zx::error(status);
  }

  // Export our InputReport protocol.
  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_input_report::InputDevice>(
      input_report_bindings_.CreateHandler(input_report_.get(), dispatcher(),
                                           fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    return result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

void InputReportDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  input_report_.reset();
  completer(zx::ok());
}

zx::result<> InputReportDriver::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("input-report");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .devfs_args(devfs.Build())
                  .Build();

  // Create endpoints of the `NodeController` for the node.
  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create endpoints: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: status %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints.client));
  node_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

}  // namespace hid_input_report_dev

FUCHSIA_DRIVER_EXPORT(hid_input_report_dev::InputReportDriver);
