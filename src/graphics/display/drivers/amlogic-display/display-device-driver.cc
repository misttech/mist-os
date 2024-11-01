// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/display-device-driver.h"

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/device_server.h>
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
#include "src/graphics/display/drivers/amlogic-display/structured_config.h"

namespace amlogic_display {

DisplayDeviceDriver::DisplayDeviceDriver(fdf::DriverStartArgs start_args,
                                         fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("amlogic-display", std::move(start_args), std::move(driver_dispatcher)) {}

void DisplayDeviceDriver::Stop() {}

zx::result<std::unique_ptr<inspect::ComponentInspector>>
DisplayDeviceDriver::CreateComponentInspector(inspect::Inspector inspector) {
  zx::result<fidl::ClientEnd<fuchsia_inspect::InspectSink>> inspect_sink_connect_result =
      incoming()->Connect<fuchsia_inspect::InspectSink>();
  if (inspect_sink_connect_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to InspectSink protocol: %s",
            inspect_sink_connect_result.status_string());
    return inspect_sink_connect_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto component_inspector = fbl::make_unique_checked<inspect::ComponentInspector>(
      &alloc_checker, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
      inspect::PublishOptions{.inspector = std::move(inspector),
                              .client_end = std::move(inspect_sink_connect_result).value()});
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for ComponentInspector");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(component_inspector));
}

zx::result<> DisplayDeviceDriver::Start() {
  auto config = take_config<structured_config::Config>();

  zx::result<std::unique_ptr<DisplayEngine>> create_display_engine_result =
      DisplayEngine::Create(incoming(), config);
  if (create_display_engine_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create DisplayEngine: %s",
            create_display_engine_result.status_string());
    return create_display_engine_result.take_error();
  }
  display_engine_ = std::move(create_display_engine_result).value();

  InitInspectorExactlyOnce(display_engine_->inspector());

  inspect::Node config_node = display_engine_->inspector().GetRoot().CreateChild("config");
  config.RecordInspect(&config_node);

  // Serves the [`fuchsia.hardware.display.controller/ControllerImpl`] protocol
  // over the compatibility server.
  banjo_server_ = compat::BanjoServer(ZX_PROTOCOL_DISPLAY_ENGINE, /*ctx=*/display_engine_.get(),
                                      /*ops=*/display_engine_->display_engine_protocol_ops());
  compat::DeviceServer::BanjoConfig banjo_config;
  banjo_config.callbacks[ZX_PROTOCOL_DISPLAY_ENGINE] = banjo_server_->callback();
  zx::result<> compat_server_init_result =
      compat_server_.Initialize(incoming(), outgoing(), node_name(), name(),
                                /*forward_metadata=*/compat::ForwardMetadata::None(),
                                /*banjo_config=*/std::move(banjo_config));
  if (compat_server_init_result.is_error()) {
    return compat_server_init_result.take_error();
  }

  const std::vector<fuchsia_driver_framework::NodeProperty> node_properties = {
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_display::BIND_PROTOCOL_ENGINE),
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
