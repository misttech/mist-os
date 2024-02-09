// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_FAKE_POWERSOURCE_DRIVER_H_
#define SRC_POWER_FAKE_POWERSOURCE_DRIVER_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <zircon/types.h>

#include <memory>
#include <string_view>

#include "power_source_protocol_server.h"
#include "simulator_impl.h"

namespace fake_powersource {

class Driver : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

 private:
  // Add a child device node and offer the service capabilities.
  template <typename A, typename B>
  zx::result<> AddDriverAndControl(std::string_view driver_node_name,
                                   std::string_view sim_node_name,
                                   driver_devfs::Connector<A>& driver_devfs_connector,
                                   driver_devfs::Connector<B>& sim_devfs_connector);
  // Add a child device node and offer the service capabilities.
  template <typename T>
  zx::result<> AddChild(std::string_view node_name, std::string_view class_name,
                        driver_devfs::Connector<T>& devfs_connector);

  // Start serving Protocol (to be called by the devfs connector when a connection is established).
  void ServeBattery(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server);

  void ServeSimulatorBattery(
      fidl::ServerEnd<fuchsia_hardware_powersource_test::SourceSimulator> server);

  // Start serving Protocol (to be called by the devfs connector when a connection is established).
  void ServeAc(fidl::ServerEnd<fuchsia_hardware_powersource::Source> server);

  void ServeSimulatorAc(fidl::ServerEnd<fuchsia_hardware_powersource_test::SourceSimulator> server);

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  std::vector<fidl::WireSyncClient<fuchsia_driver_framework::NodeController>> controllers_;

  std::shared_ptr<PowerSourceState> fake_data_battery_ =
      std::make_shared<PowerSourceState>(fuchsia_hardware_powersource::SourceInfo({
          .type = fuchsia_hardware_powersource::PowerType::kBattery,
          .state = fuchsia_hardware_powersource::kPowerStateCharging |
                   fuchsia_hardware_powersource::kPowerStateOnline,

      }));

  driver_devfs::Connector<fuchsia_hardware_powersource::Source> devfs_connector_source_battery_;
  driver_devfs::Connector<fuchsia_hardware_powersource_test::SourceSimulator>
      devfs_connector_sim_battery_;
  PowerSourceProtocolServer protocol_server_battery_;
  SimulatorImpl simulator_server_battery_;

  std::shared_ptr<PowerSourceState> fake_data_ac_ =
      std::make_shared<PowerSourceState>(fuchsia_hardware_powersource::SourceInfo({
          .type = fuchsia_hardware_powersource::PowerType::kAc,
          .state = fuchsia_hardware_powersource::kPowerStateCharging |
                   fuchsia_hardware_powersource::kPowerStateOnline,

      }));

  driver_devfs::Connector<fuchsia_hardware_powersource::Source> devfs_connector_source_ac_;
  driver_devfs::Connector<fuchsia_hardware_powersource_test::SourceSimulator>
      devfs_connector_sim_ac_;
  PowerSourceProtocolServer protocol_server_ac_;
  SimulatorImpl simulator_server_ac_;
};

}  // namespace fake_powersource

#endif  // SRC_POWER_FAKE_POWERSOURCE_DRIVER_H_
