// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "trippoint_driver.h"

#include <fidl/fuchsia.hardware.trippoint/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>

namespace fake_trippoint {

TrippointDriver::TrippointDriver(fdf::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("fake-trippoint", std::move(start_args), std::move(driver_dispatcher)),
      devfs_connector_(fit::bind_member<&TrippointDriver::Serve>(this)),
      temp_celsius_(0.0f),
      status_(ZX_OK) {}

zx::result<> TrippointDriver::Start() {
  fdf::info("Starting fake trippoint driver");
  fuchsia_hardware_trippoint::TripPointService::InstanceHandler trippoint_handler({
      .trippoint =
          trippoint_bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
  });
  zx::result<> trippoint_result =
      outgoing()->AddService<fuchsia_hardware_trippoint::TripPointService>(
          std::move(trippoint_handler));
  if (trippoint_result.is_error()) {
    fdf::error("Failed to add service: %s", trippoint_result.status_string());
    return trippoint_result.take_error();
  }

  test_trippoint::Service::InstanceHandler control_handler({
      .control = control_bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
  });
  zx::result<> control_result =
      outgoing()->AddService<test_trippoint::Service>(std::move(control_handler));
  if (control_result.is_error()) {
    fdf::error("Failed to add service: %s", control_result.status_string());
    return control_result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    fdf::error("Failed to export to devfs: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

void TrippointDriver::SetTemperatureCelsius(SetTemperatureCelsiusRequestView request,
                                            SetTemperatureCelsiusCompleter::Sync& completer) {
  temp_celsius_ = request->temp;
  status_ = request->status;
  completer.Reply();
}

void TrippointDriver::GetTemperatureCelsius(GetTemperatureCelsiusCompleter::Sync& completer) {
  completer.Reply(status_, temp_celsius_);
}

void TrippointDriver::GetSensorName(GetSensorNameCompleter::Sync& completer) {
  completer.Reply(fidl::StringView::FromExternal(std::string(name()).c_str()));
}

void TrippointDriver::GetTripPointDescriptors(GetTripPointDescriptorsCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/423948740): Implement this when clients require it.
  completer.Reply(fit::error(ZX_ERR_UNAVAILABLE));
}

void TrippointDriver::SetTripPoints(SetTripPointsRequestView request,
                                    SetTripPointsCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/423948740): Implement this when clients require it.
  completer.Reply(fit::error(ZX_ERR_UNAVAILABLE));
}

void TrippointDriver::WaitForAnyTripPoint(WaitForAnyTripPointCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/423948740): Implement this when clients require it.
  completer.Reply(fit::error(ZX_ERR_UNAVAILABLE));
}

void TrippointDriver::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_trippoint::TripPoint> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  fdf::error(
      "Unknown method in fuchsia.hardware.trippoint TripPoint protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> TrippointDriver::CreateDevfsNode() {
  fdf::info("Creating devfs node");
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    fdf::error("Error creating devfs node");
    return connector.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{
      {.connector = std::move(connector.value()),
       .class_name = "trippoint",
       .connector_supports = fuchsia_device_fs::ConnectionType::kController}};

  zx::result child = AddOwnedChild("fake-trippoint-dev", devfs_args);
  if (child.is_error()) {
    fdf::error("Failed to add owned child: %s", child.status_string());
    return child.take_error();
  }
  child_ = std::move(child.value());

  return zx::ok();
}

void TrippointDriver::Serve(fidl::ServerEnd<fuchsia_hardware_trippoint::TripPoint> request) {
  trippoint_bindings_.AddBinding(dispatcher(), std::move(request), this,
                                 fidl::kIgnoreBindingClosure);
}

}  // namespace fake_trippoint

FUCHSIA_DRIVER_EXPORT(fake_trippoint::TrippointDriver);
