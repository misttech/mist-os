// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v2/gpu-device-driver.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/legacy-bind-constants/legacy-bind-constants.h>
#include <lib/fit/defer.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/time.h>

#include <cinttypes>
#include <cstring>
#include <memory>
#include <optional>
#include <utility>

#include <bind/fuchsia/display/cpp/bind.h>

#include "lib/driver/logging/cpp/structured_logger.h"
#include "src/graphics/display/drivers/virtio-guest/v2/gpu-device.h"
#include "src/graphics/lib/virtio/virtio-abi.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace virtio_display {

namespace {

zx_status_t ResponseTypeToZxStatus(virtio_abi::ControlType type) {
  if (type != virtio_abi::ControlType::kEmptyResponse) {
    FDF_LOG(ERROR, "Unexpected response type: %s (0x%04x)", ControlTypeToString(type),
            static_cast<unsigned int>(type));
    return ZX_ERR_NO_MEMORY;
  }
  return ZX_OK;
}

}  // namespace

GpuDeviceDriver::GpuDeviceDriver(fdf::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase(name(), std::move(start_args), std::move(driver_dispatcher)) {}

GpuDeviceDriver::~GpuDeviceDriver() {}

zx_status_t GpuDeviceDriver::get_display_info() {
  const virtio_abi::GetDisplayInfoCommand command = {
      .header = {.type = virtio_abi::ControlType::kGetDisplayInfoCommand},
  };
  const auto& response = device_->ExchangeRequestResponse<virtio_abi::DisplayInfoResponse>(command);

  if (response.header.type != virtio_abi::ControlType::kDisplayInfoResponse) {
    FDF_LOG(ERROR, "Expected DisplayInfo response, got %s (0x%04x)",
            ControlTypeToString(response.header.type),
            static_cast<unsigned int>(response.header.type));
    return ZX_ERR_NOT_FOUND;
  }

  for (int i = 0; i < virtio_abi::kMaxScanouts; i++) {
    const virtio_abi::ScanoutInfo& scanout = response.scanouts[i];
    if (!scanout.enabled) {
      continue;
    }

    FDF_LOG(TRACE,
            "Scanout %d: placement (%" PRIu32 ", %" PRIu32 "), resolution %" PRIu32 "x%" PRIu32
            " flags 0x%08" PRIx32,
            i, scanout.geometry.placement_x, scanout.geometry.placement_y, scanout.geometry.width,
            scanout.geometry.height, scanout.flags);
    if (pmode_id_ >= 0) {
      continue;
    }

    // Save the first valid pmode we see.
    pmode_ = response.scanouts[i];
    pmode_id_ = i;
  }
  return ZX_OK;
}

namespace {

// Returns nullopt for an unsupported format.
std::optional<virtio_abi::ResourceFormat> To2DResourceFormat(
    fuchsia_images2::wire::PixelFormat pixel_format) {
  // TODO(https://fxbug.dev/42073721): Support more formats.
  switch (pixel_format) {
    case fuchsia_images2::PixelFormat::kB8G8R8A8:
      return virtio_abi::ResourceFormat::kBgra32;
    default:
      return std::nullopt;
  }
}

}  // namespace

zx_status_t GpuDeviceDriver::allocate_2d_resource(uint32_t* resource_id, uint32_t width,
                                                  uint32_t height,
                                                  fuchsia_images2::wire::PixelFormat pixel_format) {
  ZX_ASSERT(resource_id);

  FDF_LOG(TRACE, "Allocate2DResource");

  std::optional<virtio_abi::ResourceFormat> resource_format = To2DResourceFormat(pixel_format);
  if (!resource_format.has_value()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const virtio_abi::Create2DResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kCreate2DResourceCommand},
      .resource_id = next_resource_id_++,
      .format = virtio_abi::ResourceFormat::kBgra32,
      .width = width,
      .height = height,
  };
  *resource_id = command.resource_id;

  const auto& response = device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t GpuDeviceDriver::attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len) {
  ZX_ASSERT(ptr);

  FDF_LOG(TRACE,
          "AttachResourceBacking - resource ID %" PRIu32 ", address 0x%" PRIx64 ", length %zu",
          resource_id, ptr, buf_len);

  const virtio_abi::AttachResourceBackingCommand<1> command = {
      .header = {.type = virtio_abi::ControlType::kAttachResourceBackingCommand},
      .resource_id = resource_id,
      .entries =
          {
              {.address = ptr, .length = static_cast<uint32_t>(buf_len)},
          },
  };

  const auto& response = device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t GpuDeviceDriver::set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                                         uint32_t height) {
  FDF_LOG(TRACE,
          "SetScanout - scanout ID %" PRIu32 ", resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32,
          scanout_id, resource_id, width, height);

  const virtio_abi::SetScanoutCommand command = {
      .header = {.type = virtio_abi::ControlType::kSetScanoutCommand},
      .geometry =
          {
              .placement_x = 0,
              .placement_y = 0,
              .width = width,
              .height = height,
          },
      .scanout_id = scanout_id,
      .resource_id = resource_id,
  };

  const auto& response = device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t GpuDeviceDriver::flush_resource(uint32_t resource_id, uint32_t width, uint32_t height) {
  FDF_LOG(TRACE, "FlushResource - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32, resource_id,
          width, height);

  virtio_abi::FlushResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kFlushResourceCommand},
      .geometry = {.placement_x = 0, .placement_y = 0, .width = width, .height = height},
      .resource_id = resource_id,
  };

  const auto& response = device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t GpuDeviceDriver::transfer_to_host_2d(uint32_t resource_id, uint32_t width,
                                                 uint32_t height) {
  FDF_LOG(TRACE, "Transfer2DResourceToHost - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32,
          resource_id, width, height);

  virtio_abi::Transfer2DResourceToHostCommand command = {
      .header = {.type = virtio_abi::ControlType::kTransfer2DResourceToHostCommand},
      .geometry =
          {
              .placement_x = 0,
              .placement_y = 0,
              .width = width,
              .height = height,
          },
      .destination_offset = 0,
      .resource_id = resource_id,
  };

  const auto& response = device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

void GpuDeviceDriver::Start(fdf::StartCompleter completer) {
  FDF_LOG(TRACE, "GpuDeviceDriver::Start");

  {
    auto sysmem_result = incoming()->Connect<fuchsia_hardware_sysmem::Service::AllocatorV1>();
    if (!sysmem_result.is_ok()) {
      FDF_LOG(ERROR, "Error connecting to sysmem: %s", sysmem_result.status_string());
      completer(sysmem_result.take_error());
      return;
    }
    auto sysmem = fidl::WireSyncClient(std::move(sysmem_result.value()));
    auto pid = GetKoid(zx_process_self());
    std::string debug_name = fxl::StringPrintf("virtio-gpu-display[%lu]", pid);
    auto set_debug_status =
        sysmem->SetDebugClientInfo(fidl::StringView::FromExternal(debug_name), pid);
    if (!set_debug_status.ok()) {
      FDF_LOG(ERROR, "Cannot set sysmem allocator debug info: %s",
              set_debug_status.status_string());
      completer(zx::make_result(set_debug_status.error().status()));
      return;
    }

    sysmem_ = std::move(sysmem);
  }

  auto pci_client_end = incoming()->Connect<fuchsia_hardware_pci::Service::Device>();
  if (!pci_client_end.is_ok()) {
    FDF_LOG(ERROR, "Error requesting pci device service: %s", pci_client_end.status_string());
    completer(pci_client_end.take_error());
    return;
  }

  // Create and initialize device.
  {
    auto result = GpuDevice::Create(std::move(pci_client_end.value()));
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create device: %s", result.status_string());
      completer(result.take_error());
      return;
    }

    device_ = std::move(result.value());
  }

  // Initialize our compat server.
  {
    zx::result result = compat_server_.Initialize(incoming(), outgoing(), name(), name());
    if (result.is_error()) {
      completer(result.take_error());
      return;
    }
  }

  // Provide fuchsia.hardware.display.engine.Engine by
  // serving fuchsia.hardware.display.engine.Service.
  auto protocol =
      [this](fdf::ServerEnd<fuchsia_hardware_display_engine::Engine> server_end) mutable {
        fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(server_end), device_.get());
      };
  fuchsia_hardware_display_engine::Service::InstanceHandler handler(
      {.engine = std::move(protocol)});
  auto result =
      outgoing()->AddService<fuchsia_hardware_display_engine::Service>(std::move(handler));
  if (result.is_error()) {
    completer(result.take_error());
    return;
  }

  // Following example in driver_base.h; do all Connects and AddServices first.
  parent_node_.Bind(std::move(node()));

  // Allow adding a child node.
  fidl::Arena arena;
  auto offers = compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_display_engine::Service>(arena));

  // TODO(https://fxbug.dev/322365329): Remove this when the Display Coordinator is
  // migrated to DFv2.
  // Allow DFV1 child (display coordinator) to bind.
  auto properties = std::vector{
      fdf::MakeProperty(arena, BIND_PROTOCOL, bind_fuchsia_display::BIND_PROTOCOL_CONTROLLER_IMPL)};
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, name())
                  .offers2(offers)
                  .properties(properties)
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT(controller_endpoints.is_ok());
  auto add_result = parent_node_->AddChild(args, std::move(controller_endpoints->server), {});
  if (!add_result.ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
    completer(zx::error(add_result.status()));
    return;
  }
  controller_.Bind(std::move(controller_endpoints->client));

  auto defer_teardown = fit::defer([this]() { parent_node_ = {}; });

  defer_teardown.cancel();
  completer(zx::make_result(ZX_OK));
}

void GpuDeviceDriver::Stop() {}

void GpuDeviceDriver::PrepareStop(fdf::PrepareStopCompleter completer) { completer(zx::ok()); }

}  // namespace virtio_display

FUCHSIA_DRIVER_EXPORT(virtio_display::GpuDeviceDriver);
