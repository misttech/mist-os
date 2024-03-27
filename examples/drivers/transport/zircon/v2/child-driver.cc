// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/zircon/v2/child-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace zircon_transport {

zx::result<> ChildZirconTransportDriver::Start() {
  zx::result connect_result = incoming()->Connect<fuchsia_examples_gizmo::Service::Device>();
  if (connect_result.is_error() || !connect_result->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to gizmo service: %s", connect_result.status_string());
    return connect_result.take_error();
  }

  auto result = QueryParent(std::move(connect_result.value()));
  if (result.is_error()) {
    return result.take_error();
  }

  result = AddChild("transport-child");
  if (result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> ChildZirconTransportDriver::QueryParent(
    fidl::ClientEnd<fuchsia_examples_gizmo::Device> client_end) {
  // Query and store the hardware ID.
  auto hardware_id_result = fidl::WireCall(client_end)->GetHardwareId();
  if (!hardware_id_result.ok()) {
    FDF_SLOG(ERROR, "Failed to request hardware ID.",
             KV("status", hardware_id_result.status_string()));
    return zx::error(hardware_id_result.status());
  }
  if (hardware_id_result->is_error()) {
    FDF_SLOG(ERROR, "Hardware ID request returned an error.",
             KV("status", hardware_id_result->error_value()));
    return hardware_id_result->take_error();
  }

  hardware_id_ = hardware_id_result.value().value()->response;
  FDF_LOG(INFO, "Transport client hardware: %X", hardware_id_);

  // Query and store the firmware version.
  auto firmware_result = fidl::WireCall(client_end)->GetFirmwareVersion();
  if (!firmware_result.ok()) {
    FDF_SLOG(ERROR, "Failed to request firmware version.",
             KV("status", firmware_result.status_string()));
    return zx::error(firmware_result.status());
  }
  if (firmware_result->is_error()) {
    FDF_SLOG(ERROR, "Firmware version request returned an error.",
             KV("status", firmware_result->error_value()));
    return firmware_result->take_error();
  }

  major_version_ = firmware_result.value().value()->major;
  minor_version_ = firmware_result.value().value()->minor;
  FDF_LOG(INFO, "Transport client firmware: %d.%d", major_version_, minor_version_);

  return zx::ok();
}

zx::result<> ChildZirconTransportDriver::AddChild(std::string_view node_name) {
  fidl::Arena arena;
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, node_name).Build();

  // Create endpoints of the `NodeController` for the node.
  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed: %s", controller_endpoints.status_string());

  fidl::WireResult result =
      fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints->client), dispatcher());

  return zx::ok();
}

}  // namespace zircon_transport

FUCHSIA_DRIVER_EXPORT(zircon_transport::ChildZirconTransportDriver);
