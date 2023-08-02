// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_FAKE_BATTERY_DRIVER_H_
#define SRC_POWER_FAKE_BATTERY_DRIVER_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <zircon/types.h>

#include <memory>
#include <string_view>

#include "power_source_protocol_server.h"
#include "simulator_impl.h"

namespace fake_battery {

class Driver : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

 private:
  // Add a child device node and offer the service capabilities.
  template <typename T>
  zx::result<> AddChild(std::string_view node_name, std::string_view class_name,
                        driver_devfs::Connector<T>& devfs_connector);

  // Start serving Protocol (to be called by the devfs connector when a connection is established).
  void Serve(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server);

  void ServeSimulator(fidl::ServerEnd<fuchsia_hardware_powersource_test::SourceSimulator> server);

  std::shared_ptr<PowerSourceState> fake_data_ = nullptr;

  driver_devfs::Connector<fuchsia_hardware_powersource::Source> devfs_connector_source_;
  driver_devfs::Connector<fuchsia_hardware_powersource_test::SourceSimulator> devfs_connector_sim_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  std::vector<fidl::WireSyncClient<fuchsia_driver_framework::NodeController>> controllers_;
};

}  // namespace fake_battery

#endif  // SRC_POWER_FAKE_BATTERY_DRIVER_H_
