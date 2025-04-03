// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_TEMPERATURE_DRIVERS_AML_TRIP_AML_TRIP_H_
#define SRC_DEVICES_TEMPERATURE_DRIVERS_AML_TRIP_AML_TRIP_H_

#include <fidl/fuchsia.hardware.trippoint/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/markers.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/wire.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/wire_types.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "aml-trip-device.h"

namespace temperature {

constexpr size_t kSensorMmioIndex = 0;
constexpr size_t kTrimMmioIndex = 1;

class AmlTrip final : public fdf::DriverBase {
 public:
  AmlTrip(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("aml-trip", std::move(start_args), std::move(dispatcher)),
        devfs_connector_(fit::bind_member<&AmlTrip::Serve>(this)) {}

  // Lifecycle Management.
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;
  void Stop() override;

 private:
  // FIDL / Driver Framework Helpers
  void Serve(fidl::ServerEnd<fuchsia_hardware_trippoint::TripPoint> request);
  zx::result<> CreateDevfsNode();

  std::unique_ptr<AmlTripDevice> device_;
  fidl::ServerBindingGroup<fuchsia_hardware_trippoint::TripPoint> trippoint_bindings_;
  fdf::OwnedChildNode child_;
  driver_devfs::Connector<fuchsia_hardware_trippoint::TripPoint> devfs_connector_;
};

}  // namespace temperature

#endif  // SRC_DEVICES_TEMPERATURE_DRIVERS_AML_TRIP_AML_TRIP_H_
