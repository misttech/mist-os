// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_DFV2_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_DFV2_H_

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
#include "src/graphics/display/lib/driver-framework-migration-utils/metadata/metadata-getter.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/namespace/namespace.h"

namespace amlogic_display {

// Driver instance that binds to the amlogic-display board device.
//
// This class is responsible for interfacing with the Fuchsia Driver Framework.
class DisplayDeviceDriverDfv2 : public fdf::DriverBase {
 public:
  explicit DisplayDeviceDriverDfv2(fdf::DriverStartArgs start_args,
                                   fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~DisplayDeviceDriverDfv2() override = default;

  DisplayDeviceDriverDfv2(const DisplayDeviceDriverDfv2&) = delete;
  DisplayDeviceDriverDfv2(DisplayDeviceDriverDfv2&&) = delete;
  DisplayDeviceDriverDfv2& operator=(const DisplayDeviceDriverDfv2&) = delete;
  DisplayDeviceDriverDfv2& operator=(DisplayDeviceDriverDfv2&&) = delete;

  // fdf::DriverBase:
  zx::result<> Start() override;
  void Stop() override;

 private:
  struct DriverFrameworkMigrationUtils {
    std::unique_ptr<display::Namespace> incoming;
    std::unique_ptr<display::MetadataGetter> metadata_getter;
    std::unique_ptr<display::DispatcherFactory> dispatcher_factory;
  };

  // Creates a ComponentInspector that serves the `inspector` to the driver
  // component's Inspect sink.
  zx::result<std::unique_ptr<inspect::ComponentInspector>> CreateComponentInspector(
      inspect::Inspector inspector);

  // Creates a set of `DriverFrameworkMigrationUtils` using the resources
  // provided by the driver component.
  zx::result<DriverFrameworkMigrationUtils> CreateDriverFrameworkMigrationUtils();

  std::unique_ptr<inspect::ComponentInspector> component_inspector_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  // Must outlive `display_engine_`.
  DriverFrameworkMigrationUtils driver_framework_migration_utils_;

  // Must outlive `banjo_server_`.
  std::unique_ptr<DisplayEngine> display_engine_;

  std::optional<compat::BanjoServer> banjo_server_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_DFV2_H_
