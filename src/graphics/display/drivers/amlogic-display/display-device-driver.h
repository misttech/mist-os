// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_H_

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/zx/result.h>

#include <memory>
#include <optional>

#include "src/graphics/display/drivers/amlogic-display/display-engine.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/dispatcher-factory.h"

namespace amlogic_display {

// Driver instance that binds to the amlogic-display board device.
//
// This class is responsible for interfacing with the Fuchsia Driver Framework.
class DisplayDeviceDriver : public fdf::DriverBase {
 public:
  explicit DisplayDeviceDriver(fdf::DriverStartArgs start_args,
                               fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~DisplayDeviceDriver() override = default;

  DisplayDeviceDriver(const DisplayDeviceDriver&) = delete;
  DisplayDeviceDriver(DisplayDeviceDriver&&) = delete;
  DisplayDeviceDriver& operator=(const DisplayDeviceDriver&) = delete;
  DisplayDeviceDriver& operator=(DisplayDeviceDriver&&) = delete;

  // fdf::DriverBase:
  zx::result<> Start() override;
  void Stop() override;

 private:
  struct DriverFrameworkMigrationUtils {
    std::unique_ptr<display::DispatcherFactory> dispatcher_factory;
  };

  // Creates a set of `DriverFrameworkMigrationUtils` using the resources
  // provided by the driver component.
  zx::result<DriverFrameworkMigrationUtils> CreateDriverFrameworkMigrationUtils();

  compat::SyncInitializedDeviceServer compat_server_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  // Must outlive `display_engine_`.
  DriverFrameworkMigrationUtils driver_framework_migration_utils_;

  // Must outlive `banjo_server_`.
  std::unique_ptr<DisplayEngine> display_engine_;

  std::optional<compat::BanjoServer> banjo_server_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_H_
