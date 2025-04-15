// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_DRIVERS_PLATFORM_TEST_TEST_POWER_INTEGRATION_BOARD_H
#define SRC_DEVICES_BUS_DRIVERS_PLATFORM_TEST_TEST_POWER_INTEGRATION_BOARD_H

#include <lib/driver/component/cpp/driver_base.h>

namespace power_integration_board {

class PowerIntegrationBoard : public fdf::DriverBase {
 public:
  PowerIntegrationBoard(fdf::DriverStartArgs start_args,
                        fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("power-integration-board", std::move(start_args), std::move(dispatcher)) {}
  ~PowerIntegrationBoard() override = default;
  zx::result<> Start() override;

 private:
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};
}  // namespace power_integration_board

#endif /* SRC_DEVICES_BUS_DRIVERS_PLATFORM_TEST_TEST_POWER_INTEGRATION_BOARD_H */
