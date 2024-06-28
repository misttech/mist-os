// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/display-device-driver.h"

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/ddk/driver.h>
#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/compat/cpp/logging.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <cinttypes>
#include <cstdint>
#include <memory>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/display/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "fidl/fuchsia.driver.framework/cpp/natural_types.h"
#include "src/graphics/display/drivers/amlogic-display/display-engine.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/dispatcher-factory.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/driver-runtime-backed-dispatcher-factory.h"

namespace amlogic_display {

DisplayDeviceDriver::DisplayDeviceDriver(fdf::DriverStartArgs start_args,
                                         fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("amlogic-display", std::move(start_args), std::move(driver_dispatcher)) {}

void DisplayDeviceDriver::Stop() {}

zx::result<DisplayDeviceDriver::DriverFrameworkMigrationUtils>
DisplayDeviceDriver::CreateDriverFrameworkMigrationUtils() {
  zx::result<std::unique_ptr<display::DispatcherFactory>> create_dispatcher_factory_result =
      display::DriverRuntimeBackedDispatcherFactory::Create();
  if (create_dispatcher_factory_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create Dispatcher factory: %s",
            create_dispatcher_factory_result.status_string());
    return create_dispatcher_factory_result.take_error();
  }
  std::unique_ptr<display::DispatcherFactory> dispatcher_factory =
      std::move(create_dispatcher_factory_result).value();

  return zx::ok(DriverFrameworkMigrationUtils{
      .dispatcher_factory = std::move(dispatcher_factory),
  });
}

zx::result<> DisplayDeviceDriver::Start() {
  zx::result<DisplayDeviceDriver::DriverFrameworkMigrationUtils>
      create_driver_framework_migration_utils_result = CreateDriverFrameworkMigrationUtils();
  if (create_driver_framework_migration_utils_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create driver framework migration utilities: %s",
            create_driver_framework_migration_utils_result.status_string());
    return create_driver_framework_migration_utils_result.take_error();
  }
  driver_framework_migration_utils_ =
      std::move(create_driver_framework_migration_utils_result).value();

  zx::result<std::unique_ptr<DisplayEngine>> create_display_engine_result =
      DisplayEngine::Create(incoming(), driver_framework_migration_utils_.dispatcher_factory.get());
  if (create_display_engine_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create DisplayEngine: %s",
            create_display_engine_result.status_string());
    return create_display_engine_result.take_error();
  }
  display_engine_ = std::move(create_display_engine_result).value();

  InitInspectorExactlyOnce(display_engine_->inspector());

  // Serves the [`fuchsia.hardware.display.controller/ControllerImpl`] protocol
  // over the compatibility server.
  banjo_server_ =
      compat::BanjoServer(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL, /*ctx=*/display_engine_.get(),
                          /*ops=*/display_engine_->display_controller_impl_protocol_ops());
  compat::DeviceServer::BanjoConfig banjo_config;
  banjo_config.callbacks[ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL] = banjo_server_->callback();
  zx::result<> compat_server_init_result =
      compat_server_.Initialize(incoming(), outgoing(), node_name(), name(),
                                /*forward_metadata=*/compat::ForwardMetadata::None(),
                                /*banjo_config=*/std::move(banjo_config));
  if (compat_server_init_result.is_error()) {
    return compat_server_init_result.take_error();
  }

  const std::vector<fuchsia_driver_framework::NodeProperty> node_properties = {
      fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                        bind_fuchsia_display::BIND_PROTOCOL_CONTROLLER_IMPL),
  };
  const std::vector<fuchsia_driver_framework::Offer> node_offers = compat_server_.CreateOffers2();
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> controller_client_result =
      AddChild(name(), node_properties, node_offers);
  if (controller_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add child node: %s", controller_client_result.status_string());
    return controller_client_result.take_error();
  }
  controller_ = fidl::WireSyncClient(std::move(controller_client_result).value());

  return zx::ok();
}

}  // namespace amlogic_display

FUCHSIA_DRIVER_EXPORT(amlogic_display::DisplayDeviceDriver);
