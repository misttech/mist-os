// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-gpu-display/gpu-device-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <cstring>
#include <memory>
#include <utility>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/display/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/virtio-gpu-display/display-engine.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-banjo-adapter.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-banjo.h"

namespace virtio_display {

zx::result<> GpuDeviceDriver::InitResources() {
  fbl::AllocChecker alloc_checker;
  engine_events_ = fbl::make_unique_checked<display::DisplayEngineEventsBanjo>(&alloc_checker);
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for DisplayEngineEventsBanjo");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result sysmem_client_result = incoming()->Connect<fuchsia_sysmem2::Allocator>();
  if (sysmem_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get sysmem protocol: %s", sysmem_client_result.status_string());
    return sysmem_client_result.take_error();
  }
  fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client =
      std::move(sysmem_client_result).value();

  zx::result<fidl::ClientEnd<fuchsia_hardware_pci::Device>> pci_client_result =
      incoming()->Connect<fuchsia_hardware_pci::Service::Device>();
  if (pci_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get pci client: %s", pci_client_result.status_string());
    return pci_client_result.take_error();
  }

  zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> bti_and_backend_result =
      virtio::GetBtiAndBackend(ddk::Pci(std::move(pci_client_result).value()));
  if (!bti_and_backend_result.is_ok()) {
    FDF_LOG(ERROR, "GetBtiAndBackend failed: %s", bti_and_backend_result.status_string());
    return bti_and_backend_result.take_error();
  }
  auto [bti, backend] = std::move(bti_and_backend_result).value();

  zx::result<std::unique_ptr<DisplayEngine>> display_engine_result = DisplayEngine::Create(
      std::move(sysmem_client), std::move(bti), std::move(backend), engine_events_.get());
  if (display_engine_result.is_error()) {
    // DisplayEngine::Create() logs on error.
    return display_engine_result.take_error();
  }
  display_engine_ = std::move(display_engine_result).value();

  engine_banjo_adapter_ = fbl::make_unique_checked<display::DisplayEngineBanjoAdapter>(
      &alloc_checker, display_engine_.get(), engine_events_.get());
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for DisplayEngineBanjoAdapter");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  gpu_control_server_ = fbl::make_unique_checked<GpuControlServer>(
      &alloc_checker, this, display_engine_->pci_device().GetCapabilitySetLimit());
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for GpuControlServer");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok();
}

zx::result<> GpuDeviceDriver::InitDisplayNode() {
  // Serves the [`fuchsia.hardware.display.controller/ControllerImpl`] protocol
  // over the compatibility server.
  static constexpr std::string_view kDisplayChildNodeName = "virtio-gpu-display";
  zx::result<> compat_server_init_result =
      display_compat_server_.Initialize(incoming(), outgoing(), node_name(), kDisplayChildNodeName,
                                        /*forward_metadata=*/compat::ForwardMetadata::None(),
                                        engine_banjo_adapter_->CreateBanjoConfig());
  if (compat_server_init_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize the compatibility server: %s",
            compat_server_init_result.status_string());
    return compat_server_init_result.take_error();
  }

  const fuchsia_driver_framework::NodeProperty node_properties[] = {
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_display::BIND_PROTOCOL_ENGINE),
  };
  const std::vector<fuchsia_driver_framework::Offer> node_offers =
      display_compat_server_.CreateOffers2();
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>>
      display_node_controller_client_result =
          AddChild(kDisplayChildNodeName, node_properties, node_offers);
  if (display_node_controller_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add child node: %s",
            display_node_controller_client_result.status_string());
    return display_node_controller_client_result.take_error();
  }
  display_node_controller_ =
      fidl::WireSyncClient(std::move(display_node_controller_client_result).value());

  return zx::ok();
}

zx::result<> GpuDeviceDriver::InitGpuControlNode() {
  async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  zx::result<> add_service_result = outgoing()->AddService<fuchsia_gpu_virtio::Service>(
      gpu_control_server_->GetInstanceHandler(dispatcher),
      /*instance=*/component::kDefaultInstance);
  if (add_service_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add fuchsia.gpu.virtio service to the outgoing directory: %s",
            add_service_result.status_string());
    return add_service_result.take_error();
  }

  static constexpr std::string_view kGpuControlChildNodeName = "virtio-gpu-control";

  const fuchsia_driver_framework::Offer node_offers[] = {
      fdf::MakeOffer2<fuchsia_gpu_virtio::Service>(component::kDefaultInstance),
  };
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>>
      gpu_control_node_controller_client_result = AddChild(
          kGpuControlChildNodeName, cpp20::span<const fuchsia_driver_framework::NodeProperty>(),
          cpp20::span<const fuchsia_driver_framework::Offer>(node_offers, 1));
  if (gpu_control_node_controller_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add child node: %s",
            gpu_control_node_controller_client_result.status_string());
    return gpu_control_node_controller_client_result.take_error();
  }
  gpu_control_node_controller_ =
      fidl::WireSyncClient(std::move(gpu_control_node_controller_client_result).value());

  return zx::ok();
}

GpuDeviceDriver::GpuDeviceDriver(fdf::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("virtio-gpu-display", std::move(start_args), std::move(driver_dispatcher)) {}

GpuDeviceDriver::~GpuDeviceDriver() {}

void GpuDeviceDriver::Start(fdf::StartCompleter completer) {
  zx::result<> init_resources_result = InitResources();
  if (init_resources_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize the resources: %s", init_resources_result.status_string());
    completer(init_resources_result.take_error());
    return;
  }

  zx::result<> init_display_node_result = InitDisplayNode();
  if (init_display_node_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize the display child node: %s",
            init_display_node_result.status_string());
    completer(init_display_node_result.take_error());
    return;
  }

  zx::result<> init_gpu_control_node_result = InitGpuControlNode();
  if (init_gpu_control_node_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize the gpu control child node: %s",
            init_gpu_control_node_result.status_string());
    completer(init_gpu_control_node_result.take_error());
    return;
  }

  start_thread_ = std::thread([this, completer = std::move(completer)]() mutable {
    zx_status_t status = display_engine_->Start();
    completer(zx::make_result(status));
  });
}

void GpuDeviceDriver::Stop() {
  if (start_thread_.joinable()) {
    start_thread_.join();
  }
}

void GpuDeviceDriver::SendHardwareCommand(cpp20::span<uint8_t> request,
                                          std::function<void(cpp20::span<uint8_t>)> callback) {
  display_engine_->pci_device().ExchangeControlqVariableLengthRequestResponse(std::move(request),
                                                                              std::move(callback));
}

}  // namespace virtio_display

FUCHSIA_DRIVER_EXPORT(virtio_display::GpuDeviceDriver);
