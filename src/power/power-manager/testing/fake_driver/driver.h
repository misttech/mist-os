// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_POWER_MANAGER_TESTING_FAKE_DRIVER_DRIVER_H_
#define SRC_POWER_POWER_MANAGER_TESTING_FAKE_DRIVER_DRIVER_H_

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "temperature_driver.h"

namespace fake_driver {

class Driver : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

 private:
  // Add a child device node and offer the service capabilities.
  zx::result<> AddChild(std::string_view node_name, std::string_view class_name);

  // Start serving Protocol (to be called by the devfs connector when a connection is established).
  void Serve(fidl::ServerEnd<fuchsia_hardware_temperature::Device> server);

  driver_devfs::Connector<fuchsia_hardware_temperature::Device> devfs_connector_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  TemperatureDeviceProtocolServer temperature_server_;
  float temperature_;
};

}  // namespace fake_driver

#endif  // SRC_POWER_POWER_MANAGER_TESTING_FAKE_DRIVER_DRIVER_H_
