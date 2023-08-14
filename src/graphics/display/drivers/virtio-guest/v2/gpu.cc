// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v2/gpu.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/ddk/debug.h>
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

#include "src/graphics/display/drivers/virtio-guest/v2/virtio-abi.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace virtio {

namespace {

zx_status_t ResponseTypeToZxStatus(virtio_abi::ControlType type) {
  if (type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04x)", ControlTypeToString(type),
           static_cast<unsigned int>(type));
    return ZX_ERR_NO_MEMORY;
  }
  return ZX_OK;
}

}  // namespace

// DDK level ops

GpuDevice::GpuDevice(zx_device_t* bus_device, zx::bti bti, std::unique_ptr<Backend> backend)
    : virtio::Device(std::move(bti), std::move(backend)), DeviceType(bus_device) {
  sem_init(&request_sem_, 0, 1);
  sem_init(&response_sem_, 0, 0);
}

GpuDevice::~GpuDevice() {
  io_buffer_release(&gpu_req_);

  // TODO: clean up allocated physical memory
  sem_destroy(&request_sem_);
  sem_destroy(&response_sem_);
}

template <typename RequestType, typename ResponseType>
void GpuDevice::send_command_response(const RequestType* cmd, ResponseType** res) {
  size_t cmd_len = sizeof(RequestType);
  size_t res_len = sizeof(ResponseType);
  zxlogf(TRACE,
         "Sending command (buffer at %p, length %zu), expecting reply (pointer at %p, length %zu)",
         cmd, cmd_len, res, res_len);

  // Keep this single message at a time
  sem_wait(&request_sem_);
  auto cleanup = fit::defer([this]() { sem_post(&request_sem_); });

  uint16_t i;
  struct vring_desc* desc = vring_.AllocDescChain(2, &i);
  ZX_ASSERT(desc);

  void* gpu_req_base = io_buffer_virt(&gpu_req_);
  zx_paddr_t gpu_req_pa = io_buffer_phys(&gpu_req_);

  memcpy(gpu_req_base, cmd, cmd_len);

  desc->addr = gpu_req_pa;
  desc->len = static_cast<uint32_t>(cmd_len);
  desc->flags = VRING_DESC_F_NEXT;

  // Set the second descriptor to the response with the write bit set
  desc = vring_.DescFromIndex(desc->next);
  ZX_ASSERT(desc);

  *res = reinterpret_cast<ResponseType*>(static_cast<uint8_t*>(gpu_req_base) + cmd_len);
  zx_paddr_t res_phys = gpu_req_pa + cmd_len;
  memset(*res, 0, res_len);

  desc->addr = res_phys;
  desc->len = static_cast<uint32_t>(res_len);
  desc->flags = VRING_DESC_F_WRITE;

  // Submit the transfer & wait for the response
  vring_.SubmitChain(i);
  vring_.Kick();
  sem_wait(&response_sem_);
}

zx_status_t GpuDevice::get_display_info() {
  const virtio_abi::GetDisplayInfoCommand command = {
      .header = {.type = virtio_abi::ControlType::kGetDisplayInfoCommand},
  };

  virtio_abi::DisplayInfoResponse* response;
  send_command_response(&command, &response);
  if (response->header.type != virtio_abi::ControlType::kDisplayInfoResponse) {
    zxlogf(ERROR, "Expected DisplayInfo response, got %s (0x%04x)",
           ControlTypeToString(response->header.type),
           static_cast<unsigned int>(response->header.type));
    return ZX_ERR_NOT_FOUND;
  }

  for (int i = 0; i < virtio_abi::kMaxScanouts; i++) {
    const virtio_abi::ScanoutInfo& scanout = response->scanouts[i];
    if (!scanout.enabled) {
      continue;
    }

    zxlogf(TRACE,
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

zx_status_t GpuDevice::allocate_2d_resource(uint32_t* resource_id, uint32_t width, uint32_t height,
                                            fuchsia_images2::wire::PixelFormat pixel_format) {
  ZX_ASSERT(resource_id);

  zxlogf(TRACE, "Allocate2DResource");

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
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len) {
  ZX_ASSERT(ptr);

  zxlogf(TRACE,
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
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                                   uint32_t height) {
  zxlogf(TRACE,
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
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::flush_resource(uint32_t resource_id, uint32_t width, uint32_t height) {
  zxlogf(TRACE, "FlushResource - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32, resource_id,
         width, height);

  virtio_abi::FlushResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kFlushResourceCommand},
      .geometry = {.placement_x = 0, .placement_y = 0, .width = width, .height = height},
      .resource_id = resource_id,
  };

  virtio_abi::EmptyResponse* response;
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::transfer_to_host_2d(uint32_t resource_id, uint32_t width, uint32_t height) {
  zxlogf(TRACE, "Transfer2DResourceToHost - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32,
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
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::Start() {
  zxlogf(TRACE, "Start()");

  // Get the display info and see if we find a valid pmode
  zx_status_t status = get_display_info();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get display info: %s", zx_status_get_string(status));
    return status;
  }

  if (pmode_id_ < 0) {
    zxlogf(ERROR, "Failed to find a pmode");
    return ZX_ERR_NOT_FOUND;
  }

  zxlogf(INFO,
         "Found display at (%" PRIu32 ", %" PRIu32 ") size %" PRIu32 "x%" PRIu32
         ", flags 0x%08" PRIx32,
         pmode_.geometry.placement_x, pmode_.geometry.placement_y, pmode_.geometry.width,
         pmode_.geometry.height, pmode_.flags);

  zxlogf(TRACE, "Start() completed");
  return ZX_OK;
}

zx_status_t GpuDevice::InitSysmemAllocatorClient() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  if (!endpoints.is_ok()) {
    zxlogf(ERROR, "Cannot create sysmem allocator endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }
  auto& [client, server] = endpoints.value();
  auto connect_result = sysmem_->ConnectServer(std::move(server));
  if (!connect_result.ok()) {
    zxlogf(ERROR, "Cannot connect to sysmem Allocator protocol: %s",
           connect_result.status_string());
    return connect_result.status();
  }
  sysmem_allocator_client_ = fidl::WireSyncClient(std::move(client));

  std::string debug_name =
      fxl::StringPrintf("virtio-gpu-display[%lu]", fsl::GetCurrentProcessKoid());
  auto set_debug_status = sysmem_allocator_client_->SetDebugClientInfo(
      fidl::StringView::FromExternal(debug_name), fsl::GetCurrentProcessKoid());
  if (!set_debug_status.ok()) {
    zxlogf(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
  }

  return ZX_OK;
}

zx_status_t GpuDevice::Init() {
  zxlogf(TRACE, "Init()");

  zx::result client =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::Sysmem>("sysmem");
  if (client.is_error()) {
    zxlogf(ERROR, "Could not get Display SYSMEM protocol: %s", client.status_string());
    return client.status_value();
  }

  sysmem_ = fidl::WireSyncClient(std::move(*client));

  zx_status_t status = InitSysmemAllocatorClient();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create sysmem Allocator client: %s", zx_status_get_string(status));
    return status;
  }

  DeviceReset();

  virtio_abi::GpuDeviceConfig config;
  CopyDeviceConfig(&config, sizeof(config));
  zxlogf(TRACE, "GpuDeviceConfig - pending events: 0x%08" PRIx32, config.pending_events);
  zxlogf(TRACE, "GpuDeviceConfig - scanout limit: %d", config.scanout_limit);
  zxlogf(TRACE, "GpuDeviceConfig - capability set limit: %d", config.capability_set_limit);

  // Ack and set the driver status bit
  DriverStatusAck();

  if (!(DeviceFeaturesSupported() & VIRTIO_F_VERSION_1)) {
    // Declaring non-support until there is a need in the future.
    zxlogf(ERROR, "Legacy virtio interface is not supported by this driver");
    return ZX_ERR_NOT_SUPPORTED;
  }
  DriverFeaturesAck(VIRTIO_F_VERSION_1);
  if (zx_status_t status = DeviceStatusFeaturesOk(); status != ZX_OK) {
    zxlogf(ERROR, "Feature negotiation failed: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate the main vring
  status = vring_.Init(0, 16);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate vring: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate a GPU request
  status = io_buffer_init(&gpu_req_, bti_.get(), zx_system_get_page_size(),
                          IO_BUFFER_RW | IO_BUFFER_CONTIG);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate command buffers: %s", zx_status_get_string(status));
    return status;
  }

  zxlogf(TRACE, "Allocated command buffer at virtual address %p, physical address 0x%016" PRIx64,
         io_buffer_virt(&gpu_req_), io_buffer_phys(&gpu_req_));

  StartIrqThread();
  DriverStatusOk();

  status = DdkAdd("virtio-gpu-display");

  return status;
}

void GpuDevice::DdkInit(ddk::InitTxn txn) {
  // Start a worker thread that runs through a sequence to finish initializing the GPU
  start_thread_ = std::thread([this, init_txn = std::move(txn)]() mutable {
    zx_status_t status = Start();
    init_txn.Reply(status);
  });
}

void GpuDevice::DdkRelease() {
  if (start_thread_.joinable()) {
    start_thread_.join();
  }

  virtio::Device::Release();
}

void GpuDevice::IrqRingUpdate() {
  zxlogf(TRACE, "IrqRingUpdate()");

  // Parse our descriptor chain, add back to the free queue
  auto free_chain = [this](vring_used_elem* used_elem) {
    uint16_t i = static_cast<uint16_t>(used_elem->id);
    struct vring_desc* desc = vring_.DescFromIndex(i);
    for (;;) {
      int next;

      if (desc->flags & VRING_DESC_F_NEXT) {
        next = desc->next;
      } else {
        // End of chain
        next = -1;
      }

      vring_.FreeDesc(i);

      if (next < 0) {
        break;
      }
      i = static_cast<uint16_t>(next);
      desc = vring_.DescFromIndex(i);
    }
    // Notify the request thread
    sem_post(&response_sem_);
  };

  // Tell the ring to find free chains and hand it back to our lambda
  vring_.IrqRingUpdate(free_chain);
}

void GpuDevice::IrqConfigChange() { zxlogf(TRACE, "IrqConfigChange()"); }

}  // namespace virtio
