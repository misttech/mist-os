// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/virtio/driver_utils.h>

#include "gpu.h"

namespace virtio {

GpuDriver::Device::Device(zx::bti bti, std::unique_ptr<Backend> backend)
    : virtio::Device(std::move(bti), std::move(backend)) {
  sem_init(&request_sem_, 0, 1);
  sem_init(&response_sem_, 0, 0);
}

GpuDriver::Device::~Device() {
  if (request_virt_addr_) {
    zx::vmar::root_self()->unmap(request_virt_addr_, GetRequestSize(request_vmo_));
  }

  sem_destroy(&request_sem_);
  sem_destroy(&response_sem_);
}

fit::result<zx_status_t, std::unique_ptr<GpuDriver::Device>> GpuDriver::Device::Create(
    fidl::ClientEnd<fuchsia_hardware_pci::Device> client_end) {
  zx::bti bti;
  std::unique_ptr<virtio::Backend> backend;
  {
    auto result = GetBtiAndBackend(ddk::Pci(std::move(client_end)));
    if (!result.is_ok()) {
      FDF_LOG(ERROR, "GetBtiAndBackend failed");
      return result.take_error();
    }
    bti = std::move(result.value().first);
    backend = std::move(result.value().second);
  }

  auto device = std::make_unique<Device>(std::move(bti), std::move(backend));
  if (zx_status_t status = device->Init(); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize device");
    return zx::error(status);
  }

  return zx::ok(std::move(device));
}

zx_status_t GpuDriver::Device::Init() {
  DeviceReset();

  virtio_abi::GpuDeviceConfig config;
  CopyDeviceConfig(&config, sizeof(config));
  FDF_LOG(TRACE, "GpuDeviceConfig - pending events: 0x%08" PRIx32, config.pending_events);
  FDF_LOG(TRACE, "GpuDeviceConfig - scanout limit: %d", config.scanout_limit);
  FDF_LOG(TRACE, "GpuDeviceConfig - capability set limit: %d", config.capability_set_limit);

  // Ack and set the driver status bit
  DriverStatusAck();

  if (!(DeviceFeaturesSupported() & VIRTIO_F_VERSION_1)) {
    // Declaring non-support until there is a need in the future.
    FDF_LOG(ERROR, "Legacy virtio interface is not supported by this driver");
    return ZX_ERR_NOT_SUPPORTED;
  }

  DriverFeaturesAck(VIRTIO_F_VERSION_1);

  zx_status_t status = DeviceStatusFeaturesOk();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Feature negotiation failed: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate the main vring
  status = vring_.Init(0, 16);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to allocate vring: %s", zx_status_get_string(status));
    return status;
  }

  status = zx::vmo::create(zx_system_get_page_size(), /*options=*/0, &request_vmo_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to allocate request vmo: %s", zx_status_get_string(status));
    return status;
  }

  size_t size = GetRequestSize(request_vmo_);

  // Commit backing store and get the physical address.
  status = bti().pin(ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, request_vmo_, /*offset=*/0, size,
                     &request_phys_addr_, 1, &request_pmt_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to pin request vmo: %s", zx_status_get_string(status));
    return status;
  }

  status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, /*vmar_offset=*/0,
                                      request_vmo_,
                                      /*vmo_offset=*/0, size, &request_virt_addr_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map request vmo: %s", zx_status_get_string(status));
    return status;
  }

  FDF_LOG(INFO,
          "Allocated command buffer at virtual address 0x%016" PRIx64
          ", physical address 0x%016" PRIx64,
          request_virt_addr_, request_phys_addr_);

  StartIrqThread();
  DriverStatusOk();

  return ZX_OK;
}

// static
uint64_t GpuDriver::Device::GetRequestSize(zx::vmo& vmo) {
  uint64_t size = 0;
  ZX_ASSERT(ZX_OK == vmo.get_size(&size));
  return size;
}

void GpuDriver::Device::IrqRingUpdate() {
  FDF_LOG(TRACE, "IrqRingUpdate()");

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

void GpuDriver::Device::IrqConfigChange() { FDF_LOG(TRACE, "IrqConfigChange()"); }

}  // namespace virtio
