// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_FAKE_BATTERY_DRIVER_H_
#define SRC_POWER_FAKE_BATTERY_DRIVER_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <zircon/types.h>

#include <string_view>

#include "power_source_protocol_server.h"

namespace fake_battery {

class Driver : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

 private:
  // Add a child device node and offer the service capabilities.
  zx::result<> AddChild(std::string_view node_name);

  // Start serving Protocol (to be called by the devfs connector when a connection is established).
  void Serve(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server);

  driver_devfs::Connector<fuchsia_hardware_powersource::Source> devfs_connector_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace fake_battery

#endif  // SRC_POWER_FAKE_BATTERY_DRIVER_H_
