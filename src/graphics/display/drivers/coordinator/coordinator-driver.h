// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_COORDINATOR_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_COORDINATOR_DRIVER_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/driver/compat/cpp/banjo_client.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display_coordinator {

// Interfaces with the Driver Framework v2 and manages the driver lifetime of
// the display coordinator Controller device.
class CoordinatorDriver : public fdf::DriverBase {
 public:
  CoordinatorDriver(fdf::DriverStartArgs start_args,
                    fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  ~CoordinatorDriver() override;

  CoordinatorDriver(const CoordinatorDriver&) = delete;
  CoordinatorDriver(CoordinatorDriver&&) = delete;
  CoordinatorDriver& operator=(const CoordinatorDriver&) = delete;
  CoordinatorDriver& operator=(CoordinatorDriver&&) = delete;

  // fdf::DriverBase:
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;
  void Stop() override;

  Controller* controller() const { return controller_.get(); }

 private:
  void ConnectProvider(fidl::ServerEnd<fuchsia_hardware_display::Provider> provider_request);

  fdf::SynchronizedDispatcher client_dispatcher_;
  std::unique_ptr<Controller> controller_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;

  fidl::ServerBindingGroup<fuchsia_hardware_display::Provider> provider_bindings_;
  driver_devfs::Connector<fuchsia_hardware_display::Provider> devfs_connector_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_COORDINATOR_DRIVER_H_
