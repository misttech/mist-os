// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/driver/v2/child-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace driver_transport {

zx::result<> ChildDriverTransportDriver::Start() {
  // Connect to the `fuchsia.examples.gizmo.Service` provided by the parent.
  auto connect_result = incoming()->Connect<fuchsia_examples_gizmo::Service::Device>();
  if (connect_result.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect gizmo device protocol.",
             KV("status", connect_result.status_string()));
    return connect_result.take_error();
  }

  auto result = QueryParent(std::move(connect_result.value()));
  if (result.is_error()) {
    return result.take_error();
  }

  zx::result child_result = AddChild("transport-child", {}, {});
  if (child_result.is_error()) {
    return child_result.take_error();
  }

  controller_.Bind(std::move(child_result.value()), dispatcher());

  // Since we set the dispatcher to "ALLOW_SYNC_CALLS" in the driver CML, we
  // need to seal the option after we finish all our sync calls over driver transport.
  auto status =
      fdf_dispatcher_seal(driver_dispatcher()->get(), FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to seal ALLOW_SYNC_CALLS.", KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> ChildDriverTransportDriver::QueryParent(
    fdf::ClientEnd<fuchsia_examples_gizmo::Device> client_end) {
  fdf::Arena arena('GIZM');

  // Query and store the hardware ID.
  auto hardware_id_result = fdf::WireCall(client_end).buffer(arena)->GetHardwareId();
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
  auto firmware_result = fdf::WireCall(client_end).buffer(arena)->GetFirmwareVersion();
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

}  // namespace driver_transport

FUCHSIA_DRIVER_EXPORT(driver_transport::ChildDriverTransportDriver);
