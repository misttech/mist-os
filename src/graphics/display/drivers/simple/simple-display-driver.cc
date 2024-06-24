// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/simple/simple-display-driver.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/mmio/mmio-buffer.h>

#include <memory>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/display/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/simple/simple-display.h"

namespace simple_display {

SimpleDisplayDriver::SimpleDisplayDriver(std::string_view device_name,
                                         fdf::DriverStartArgs start_args,
                                         fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase(device_name, std::move(start_args), std::move(driver_dispatcher)) {}

SimpleDisplayDriver::~SimpleDisplayDriver() = default;

zx::result<> SimpleDisplayDriver::Start() {
  zx::result<> configure_hardware_result = ConfigureHardware();
  if (configure_hardware_result.is_error()) {
    FDF_LOG(ERROR, "Failed to configure hardware: %s", configure_hardware_result.status_string());
    return configure_hardware_result.take_error();
  }

  zx::result<std::unique_ptr<SimpleDisplay>> simple_display_result =
      CreateAndInitializeSimpleDisplay();
  if (simple_display_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create and initialize SimpleDisplay: %s",
            simple_display_result.status_string());
    return simple_display_result.take_error();
  }
  simple_display_ = std::move(simple_display_result).value();

  zx::result<> add_banjo_server_node_result = InitializeBanjoServerNode();
  if (add_banjo_server_node_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add banjo server node: %s",
            add_banjo_server_node_result.status_string());
    return add_banjo_server_node_result.take_error();
  }

  return zx::ok();
}

void SimpleDisplayDriver::Stop() {}

zx::result<std::unique_ptr<SimpleDisplay>> SimpleDisplayDriver::CreateAndInitializeSimpleDisplay() {
  zx::result<fdf::MmioBuffer> frame_buffer_mmio_result = GetFrameBufferMmioBuffer();
  if (frame_buffer_mmio_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get frame buffer mmio buffer: %s",
            frame_buffer_mmio_result.status_string());
    return frame_buffer_mmio_result.take_error();
  }
  fdf::MmioBuffer frame_buffer_mmio = std::move(frame_buffer_mmio_result).value();

  zx::result<DisplayProperties> display_properties_result = GetDisplayProperties();
  if (display_properties_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get display properties: %s",
            display_properties_result.status_string());
    return display_properties_result.take_error();
  }
  DisplayProperties display_properties = std::move(display_properties_result).value();

  zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>> hardware_sysmem_result =
      incoming()->Connect<fuchsia_hardware_sysmem::Service::Sysmem>("sysmem");
  if (hardware_sysmem_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get hardware sysmem protocol: %s",
            hardware_sysmem_result.status_string());
    return hardware_sysmem_result.take_error();
  }
  fidl::WireSyncClient hardware_sysmem{std::move(hardware_sysmem_result).value()};

  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> sysmem_result =
      incoming()->Connect<fuchsia_hardware_sysmem::Service::AllocatorV2>("sysmem");
  if (sysmem_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get fuchsia.sysmem2.Allocator protocol: %s",
            sysmem_result.status_string());
    return sysmem_result.take_error();
  }
  fidl::WireSyncClient sysmem(std::move(sysmem_result).value());

  fbl::AllocChecker alloc_checker;
  auto simple_display = fbl::make_unique_checked<SimpleDisplay>(
      &alloc_checker, std::move(hardware_sysmem), std::move(sysmem), std::move(frame_buffer_mmio),
      std::move(display_properties));
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for SimpleDisplay");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> simple_display_initialize_result = simple_display->Initialize();
  if (simple_display_initialize_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize SimpleDisplay: %s",
            simple_display_initialize_result.status_string());
    return simple_display_initialize_result.take_error();
  }

  return zx::ok(std::move(simple_display));
}

zx::result<> SimpleDisplayDriver::InitializeBanjoServerNode() {
  ZX_DEBUG_ASSERT(simple_display_ != nullptr);
  display_controller_impl_protocol_t protocol = simple_display_->GetProtocol();

  // Serves the [`fuchsia.hardware.display.controller/ControllerImpl`] protocol
  // over the compatibility server.
  banjo_server_ =
      compat::BanjoServer(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL, protocol.ctx, protocol.ops);
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

}  // namespace simple_display
