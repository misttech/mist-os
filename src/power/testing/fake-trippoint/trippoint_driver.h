// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_TESTING_FAKE_TRIPPOINT_TRIPPOINT_DRIVER_H_
#define SRC_POWER_TESTING_FAKE_TRIPPOINT_TRIPPOINT_DRIVER_H_

#include <fidl/fuchsia.hardware.trippoint/cpp/fidl.h>
#include <fidl/test.trippoint/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <zircon/status.h>

namespace fake_trippoint {

class TrippointDriver : public fdf::DriverBase,
                        public fidl::WireServer<fuchsia_hardware_trippoint::TripPoint>,
                        public fidl::WireServer<test_trippoint::Control> {
 public:
  TrippointDriver(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  zx::result<> Start() override;

  // Implements test.temperature
  void SetTemperatureCelsius(SetTemperatureCelsiusRequestView request,
                             SetTemperatureCelsiusCompleter::Sync& completer) override;

  // Implements fuchsia.hardware.temperature
  void GetTemperatureCelsius(GetTemperatureCelsiusCompleter::Sync& completer) override;
  void GetSensorName(GetSensorNameCompleter::Sync& completer) override;

  // Implements fuchsia.hardware.trippoint
  void GetTripPointDescriptors(GetTripPointDescriptorsCompleter::Sync& completer) override;
  void SetTripPoints(SetTripPointsRequestView request,
                     SetTripPointsCompleter::Sync& completer) override;
  void WaitForAnyTripPoint(WaitForAnyTripPointCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_trippoint::TripPoint> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  zx::result<> CreateDevfsNode();
  void Serve(fidl::ServerEnd<fuchsia_hardware_trippoint::TripPoint> request);

  driver_devfs::Connector<fuchsia_hardware_trippoint::TripPoint> devfs_connector_;
  fidl::ServerBindingGroup<fuchsia_hardware_trippoint::TripPoint> trippoint_bindings_;
  fidl::ServerBindingGroup<test_trippoint::Control> control_bindings_;

  fdf::OwnedChildNode child_;
  float temp_celsius_;
  zx_status_t status_;
};

}  // namespace fake_trippoint

#endif  // SRC_POWER_TESTING_FAKE_TRIPPOINT_TRIPPOINT_DRIVER_H_
