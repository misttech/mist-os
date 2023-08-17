// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v2/gpu.h"

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/driver/component/cpp/driver_export.h>
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

#include "src/graphics/display/drivers/virtio-guest/v2/device.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace virtio {

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

// DDK level ops

GpuDriver::GpuDriver(fdf::DriverStartArgs start_args,
                     fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("virtio-gpu", std::move(start_args), std::move(driver_dispatcher)) {}

GpuDriver::~GpuDriver() {}

zx_status_t GpuDriver::get_display_info() {
  const virtio_abi::GetDisplayInfoCommand command = {
      .header = {.type = virtio_abi::ControlType::kGetDisplayInfoCommand},
  };

  virtio_abi::DisplayInfoResponse* response;
  device_->send_command_response(&command, &response);

  if (response->header.type != virtio_abi::ControlType::kDisplayInfoResponse) {
    FDF_LOG(ERROR, "Expected DisplayInfo response, got %s (0x%04x)",
            ControlTypeToString(response->header.type),
            static_cast<unsigned int>(response->header.type));
    return ZX_ERR_NOT_FOUND;
  }

  for (int i = 0; i < virtio_abi::kMaxScanouts; i++) {
    const virtio_abi::ScanoutInfo& scanout = response->scanouts[i];
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

    // Save the first valid pmode we see
    pmode_ = response->scanouts[i];
    pmode_id_ = i;
  }
  return ZX_OK;
}

namespace {

// Returns nullopt for an unsupported format.
std::optional<virtio_abi::ResourceFormat> To2DResourceFormat(
    fuchsia_images2::wire::PixelFormat pixel_format) {
  // TODO(fxbug.dev/122802): Support more formats.
  switch (pixel_format) {
    case fuchsia_images2::PixelFormat::kBgra32:
      return virtio_abi::ResourceFormat::kBgra32;
    default:
      return std::nullopt;
  }
}

}  // namespace

zx_status_t GpuDriver::allocate_2d_resource(uint32_t* resource_id, uint32_t width, uint32_t height,
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

  virtio_abi::EmptyResponse* response;
  device_->send_command_response(&command, &response);

  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDriver::attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len) {
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

  virtio_abi::EmptyResponse* response;
  device_->send_command_response(&command, &response);

  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDriver::set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
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

  virtio_abi::EmptyResponse* response;
  device_->send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDriver::flush_resource(uint32_t resource_id, uint32_t width, uint32_t height) {
  FDF_LOG(TRACE, "FlushResource - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32, resource_id,
          width, height);

  virtio_abi::FlushResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kFlushResourceCommand},
      .geometry = {.placement_x = 0, .placement_y = 0, .width = width, .height = height},
      .resource_id = resource_id,
  };

  virtio_abi::EmptyResponse* response;
  device_->send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDriver::transfer_to_host_2d(uint32_t resource_id, uint32_t width, uint32_t height) {
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

  virtio_abi::EmptyResponse* response;
  device_->send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDriver::Stage2Init() {
  FDF_LOG(TRACE, "Stage2Init()");

  // Get the display info and see if we find a valid pmode
  zx_status_t status = get_display_info();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to get display info: %s", zx_status_get_string(status));
    return status;
  }

  if (pmode_id_ < 0) {
    FDF_LOG(ERROR, "Failed to find a pmode");
    return ZX_ERR_NOT_FOUND;
  }

  FDF_LOG(INFO,
          "Found display at (%" PRIu32 ", %" PRIu32 ") size %" PRIu32 "x%" PRIu32
          ", flags 0x%08" PRIx32,
          pmode_.geometry.placement_x, pmode_.geometry.placement_y, pmode_.geometry.width,
          pmode_.geometry.height, pmode_.flags);

  FDF_LOG(TRACE, "Start() completed");
  return ZX_OK;
}

zx_status_t GpuDriver::InitSysmemAllocatorClient() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  if (!endpoints.is_ok()) {
    FDF_LOG(ERROR, "Cannot create sysmem allocator endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }
  auto& [client, server] = endpoints.value();
  auto connect_result = sysmem_->ConnectServer(std::move(server));
  if (!connect_result.ok()) {
    FDF_LOG(ERROR, "Cannot connect to sysmem Allocator protocol: %s",
            connect_result.status_string());
    return connect_result.status();
  }
  sysmem_allocator_client_ = fidl::WireSyncClient(std::move(client));

  std::string debug_name =
      fxl::StringPrintf("virtio-gpu-display[%lu]", fsl::GetCurrentProcessKoid());
  auto set_debug_status = sysmem_allocator_client_->SetDebugClientInfo(
      fidl::StringView::FromExternal(debug_name), fsl::GetCurrentProcessKoid());
  if (!set_debug_status.ok()) {
    FDF_LOG(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
  }

  return ZX_OK;
}

void GpuDriver::Start(fdf::StartCompleter completer) {
  FDF_LOG(TRACE, "GpuDriver::Start");

  {
    auto sysmem_client_end = incoming()->Connect<fuchsia_hardware_sysmem::Service::Sysmem>();
    if (!sysmem_client_end.is_ok()) {
      FDF_LOG(ERROR, "Error connecting to sysmem: %s", sysmem_client_end.status_string());
      completer(sysmem_client_end.take_error());
      return;
    }

    sysmem_ = fidl::WireSyncClient(std::move(sysmem_client_end.value()));
  }

  auto pci_client_end = incoming()->Connect<fuchsia_hardware_pci::Service::Device>();
  if (!pci_client_end.is_ok()) {
    FDF_LOG(ERROR, "Error requesting pci device service: %s", pci_client_end.status_string());
    completer(pci_client_end.take_error());
    return;
  }

  // Following example in driver_base.h; do all Connects and AddServices first.
  parent_node_.Bind(std::move(node()));

  auto defer_teardown = fit::defer([this]() { parent_node_ = {}; });

  if (zx_status_t status = InitSysmemAllocatorClient(); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create sysmem Allocator client: %s", zx_status_get_string(status));
    completer(zx::error(status));
    return;
  }

  {
    // Create and initialize device
    auto result = Device::Create(std::move(pci_client_end.value()));
    if (result.is_error()) {
      completer(result.take_error());
    }

    device_ = std::move(result.value());
  }

  defer_teardown.cancel();

  async::PostTask(dispatcher(), [this, completer = std::move(completer)]() mutable {
    zx_status_t status = Stage2Init();
    completer(zx::make_result(status));
  });
}

void GpuDriver::Stop() { device_->Release(); }

}  // namespace virtio

FUCHSIA_DRIVER_EXPORT(virtio::GpuDriver);

// TODO(b/282968393): remove when libdriver dep removed from bus/lib/virtio
zx_driver_rec_t __zircon_driver_rec__ = {};
