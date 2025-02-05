// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-display/intel-display-driver.h"

#include <fidl/fuchsia.boot/cpp/fidl.h>
#include <fidl/fuchsia.hardware.pci/cpp/fidl.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fidl/fuchsia.system.state/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/prepare_stop_completer.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <cstdint>
#include <memory>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/display/cpp/bind.h>
#include <bind/fuchsia/intel/platform/gpucore/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/intel-display/intel-display.h"

namespace intel_display {

namespace {

zx::result<fuchsia_system_state::SystemPowerState> GetSystemPowerState(fdf::Namespace& incoming) {
  zx::result<fidl::ClientEnd<fuchsia_system_state::SystemStateTransition>>
      sysmem_power_transition_result =
          incoming.Connect<fuchsia_system_state::SystemStateTransition>();
  if (sysmem_power_transition_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to fuchsia.system.state/SystemStateTransition: %s",
            sysmem_power_transition_result.status_string());
    return sysmem_power_transition_result.take_error();
  }
  fidl::WireSyncClient system_power_transition(std::move(sysmem_power_transition_result).value());

  fidl::WireResult termination_state_result = system_power_transition->GetTerminationSystemState();
  if (!termination_state_result.ok()) {
    FDF_LOG(ERROR, "Failed to get termination state: %s", termination_state_result.status_string());
    return zx::error(termination_state_result.status());
  }
  return zx::ok(termination_state_result.value().state);
}

template <typename ResourceProtocol>
zx::result<zx::resource> GetKernelResource(fdf::Namespace& incoming, const char* resource_name) {
  zx::result resource_result = incoming.Connect<ResourceProtocol>();
  if (resource_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to kernel %s resource: %s", resource_name,
            resource_result.status_string());
    return resource_result.take_error();
  }
  fidl::WireSyncClient<ResourceProtocol> resource =
      fidl::WireSyncClient(std::move(resource_result).value());
  fidl::WireResult<typename ResourceProtocol::Get> get_result = resource->Get();
  if (!get_result.ok()) {
    FDF_LOG(ERROR, "Failed to get kernel %s resource: %s", resource_name,
            get_result.FormatDescription().c_str());
    return zx::error(get_result.status());
  }
  return zx::ok(std::move(std::move(get_result).value()).resource);
}

zx::result<zbi_swfb_t> GetFramebufferInfo(fdf::Namespace& incoming) {
  zx::result client = incoming.Connect<fuchsia_boot::Items>();
  if (client.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to fuchsia.boot/Items: %s", client.status_string());
    return client.take_error();
  }
  fidl::WireResult result = fidl::WireCall(*client)->Get2(ZBI_TYPE_FRAMEBUFFER, {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to get framebuffer boot item: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  fidl::VectorView items = result->value()->retrieved_items;
  if (items.count() == 0) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  if (items[0].length < sizeof(zbi_swfb_t)) {
    return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
  }
  zbi_swfb_t framebuffer_info;
  zx_status_t status = items[0].payload.read(&framebuffer_info, 0, sizeof(framebuffer_info));
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(framebuffer_info);
}

}  // namespace

IntelDisplayDriver::IntelDisplayDriver(fdf::DriverStartArgs start_args,
                                       fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("intel-display", std::move(start_args), std::move(driver_dispatcher)) {}

IntelDisplayDriver::~IntelDisplayDriver() = default;

zx::result<> IntelDisplayDriver::InitController() {
  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> sysmem_result =
      incoming()->Connect<fuchsia_sysmem2::Allocator>();
  if (sysmem_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to sysmem protocol: %s", sysmem_result.status_string());
    return sysmem_result.take_error();
  }
  fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem = std::move(sysmem_result).value();

  zx::result<fidl::ClientEnd<fuchsia_hardware_pci::Device>> pci_result =
      incoming()->Connect<fuchsia_hardware_pci::Service::Device>("pci");
  if (pci_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to pci protocol: %s", pci_result.status_string());
    return pci_result.take_error();
  }
  fidl::ClientEnd<fuchsia_hardware_pci::Device> pci = std::move(pci_result).value();

  zx::result<zbi_swfb_t> framebuffer_info = GetFramebufferInfo(*incoming());
  if (framebuffer_info.is_ok()) {
    framebuffer_info_ = framebuffer_info.value();
  }

  zx::result<zx::resource> mmio_resource_result =
      GetKernelResource<fuchsia_kernel::MmioResource>(*incoming(), "mmio");
  if (mmio_resource_result.is_error()) {
    return mmio_resource_result.take_error();
  }
  mmio_resource_ = std::move(mmio_resource_result).value();

  zx::result<zx::resource> ioport_resource_result =
      GetKernelResource<fuchsia_kernel::IoportResource>(*incoming(), "ioport");
  if (ioport_resource_result.is_error()) {
    return ioport_resource_result.take_error();
  }
  ioport_resource_ = std::move(ioport_resource_result).value();

  ControllerResources resources = {
      .mmio = mmio_resource_.borrow(),
      .ioport = ioport_resource_.borrow(),
  };

  zx::result<std::unique_ptr<Controller>> controller_result =
      Controller::Create(std::move(sysmem), std::move(pci), std::move(resources), framebuffer_info_,
                         inspector().inspector());
  if (controller_result.is_error()) {
    return controller_result.take_error();
  }
  controller_ = std::move(controller_result).value();

  return zx::ok();
}

void IntelDisplayDriver::Start(fdf::StartCompleter completer) {
  zx::result<> init_controller_result = InitController();
  if (init_controller_result.is_error()) {
    completer(init_controller_result.take_error());
    return;
  }

  zx::result<> init_display_node_result = InitDisplayNode();
  if (init_display_node_result.is_error()) {
    completer(init_display_node_result.take_error());
    return;
  }

  zx::result<> init_gpu_core_node_result = InitGpuCoreNode();
  if (init_gpu_core_node_result.is_error()) {
    completer(init_gpu_core_node_result.take_error());
    return;
  }

  controller_->Start(std::move(completer));
}

void IntelDisplayDriver::PrepareStopOnPowerOn(fdf::PrepareStopCompleter completer) {
  if (gpu_core_node_controller_.is_valid()) {
    fidl::OneWayStatus remove_gpu_result = gpu_core_node_controller_->Remove();
    if (!remove_gpu_result.ok()) {
      FDF_LOG(WARNING, "Failed to remove gpu core node: %s", remove_gpu_result.status_string());
    }
  }

  if (display_node_controller_.is_valid()) {
    fidl::OneWayStatus remove_display_result = display_node_controller_->Remove();
    if (!remove_display_result.ok()) {
      FDF_LOG(WARNING, "Failed to remove display node: %s", remove_display_result.status_string());
    }
  }

  if (controller_) {
    controller_->PrepareStopOnPowerOn(std::move(completer));
  } else {
    completer(zx::ok());
  }
}

void IntelDisplayDriver::PrepareStopOnPowerStateTransition(
    fuchsia_system_state::SystemPowerState power_state, fdf::PrepareStopCompleter completer) {
  if (controller_) {
    controller_->PrepareStopOnPowerStateTransition(power_state, std::move(completer));
  } else {
    completer(zx::ok());
  }
}

void IntelDisplayDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  zx::result<fuchsia_system_state::SystemPowerState> system_power_state_result =
      GetSystemPowerState(*incoming());
  if (system_power_state_result.is_error()) {
    FDF_LOG(WARNING, "Failed to get system power state: %s, fallback to fully on",
            system_power_state_result.status_string());
  }

  fuchsia_system_state::SystemPowerState system_power_state =
      system_power_state_result.value_or(fuchsia_system_state::SystemPowerState::kFullyOn);

  if (system_power_state == fuchsia_system_state::SystemPowerState::kFullyOn) {
    PrepareStopOnPowerOn(std::move(completer));
    return;
  }
  PrepareStopOnPowerStateTransition(system_power_state, std::move(completer));
}

void IntelDisplayDriver::Stop() {}

zx::result<ddk::AnyProtocol> IntelDisplayDriver::GetProtocol(uint32_t proto_id) {
  return controller_->GetProtocol(proto_id);
}

zx::result<> IntelDisplayDriver::InitDisplayNode() {
  ZX_DEBUG_ASSERT(!display_node_controller_.is_valid());

  // Serves the [`fuchsia.hardware.display.controller/ControllerImpl`] protocol
  // over the compatibility server.
  zx::result<ddk::AnyProtocol> protocol_result =
      controller_->GetProtocol(ZX_PROTOCOL_DISPLAY_ENGINE);
  ZX_DEBUG_ASSERT(protocol_result.is_ok());
  ddk::AnyProtocol protocol = std::move(protocol_result).value();

  display_banjo_server_ =
      compat::BanjoServer(ZX_PROTOCOL_DISPLAY_ENGINE, protocol.ctx, protocol.ops);
  compat::DeviceServer::BanjoConfig banjo_config;
  banjo_config.callbacks[ZX_PROTOCOL_DISPLAY_ENGINE] = display_banjo_server_->callback();

  static constexpr std::string_view kDisplayChildNodeName = "intel-display-controller";
  zx::result<> compat_server_init_result =
      display_compat_server_.Initialize(incoming(), outgoing(), node_name(), kDisplayChildNodeName,
                                        /*forward_metadata=*/compat::ForwardMetadata::None(),
                                        /*banjo_config=*/std::move(banjo_config));
  if (compat_server_init_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize the compatibility server: %s",
            compat_server_init_result.status_string());
    return compat_server_init_result.take_error();
  }

  const std::vector<fuchsia_driver_framework::NodeProperty> node_properties = {
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

zx::result<> IntelDisplayDriver::InitGpuCoreNode() {
  ZX_DEBUG_ASSERT(!gpu_core_node_controller_.is_valid());

  // Serves the [`fuchsia.hardware.intelgpucore/IntelGpuCore`] protocol
  // over the compatibility server.
  zx::result<ddk::AnyProtocol> protocol_result =
      controller_->GetProtocol(ZX_PROTOCOL_INTEL_GPU_CORE);
  ZX_DEBUG_ASSERT(protocol_result.is_ok());
  ddk::AnyProtocol protocol = std::move(protocol_result).value();

  gpu_banjo_server_ = compat::BanjoServer(ZX_PROTOCOL_INTEL_GPU_CORE, protocol.ctx, protocol.ops);
  compat::DeviceServer::BanjoConfig banjo_config;
  banjo_config.callbacks[ZX_PROTOCOL_INTEL_GPU_CORE] = gpu_banjo_server_->callback();

  static constexpr std::string_view kGpuCoreChildNodeName = "intel-gpu-core";
  zx::result<> compat_server_init_result =
      gpu_compat_server_.Initialize(incoming(), outgoing(), node_name(), kGpuCoreChildNodeName,
                                    /*forward_metadata=*/compat::ForwardMetadata::None(),
                                    /*banjo_config=*/std::move(banjo_config));
  if (compat_server_init_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize the compatibility server: %s",
            compat_server_init_result.status_string());
    return compat_server_init_result.take_error();
  }

  const std::vector<fuchsia_driver_framework::NodeProperty> node_properties = {
      fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                        bind_fuchsia_intel_platform_gpucore::BIND_PROTOCOL_DEVICE),
  };
  const std::vector<fuchsia_driver_framework::Offer> node_offers =
      gpu_compat_server_.CreateOffers2();
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>>
      gpu_core_node_controller_client_result =
          AddChild(kGpuCoreChildNodeName, node_properties, node_offers);
  if (gpu_core_node_controller_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add child node: %s",
            gpu_core_node_controller_client_result.status_string());
    return gpu_core_node_controller_client_result.take_error();
  }

  gpu_core_node_controller_ =
      fidl::WireSyncClient(std::move(gpu_core_node_controller_client_result).value());
  return zx::ok();
}

}  // namespace intel_display

FUCHSIA_DRIVER_EXPORT(intel_display::IntelDisplayDriver);
