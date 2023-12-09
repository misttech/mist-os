// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v2/gpu-device.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/stdcompat/span.h>
#include <lib/virtio/backends/backend.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zx/pmt.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/virtio-guest/v2/virtio-abi.h"

namespace virtio_display {

GpuDevice::GpuDevice(zx::bti bti, std::unique_ptr<virtio::Backend> backend,
                     zx::vmo virtio_queue_buffer_pool_vmo, zx::pmt virtio_queue_buffer_pool_pin,
                     zx_paddr_t virtio_queue_buffer_pool_physical_address,
                     cpp20::span<uint8_t> virtio_queue_buffer_pool)
    : virtio::Device(std::move(bti), std::move(backend)),
      virtio_queue_(this),
      virtio_queue_buffer_pool_vmo_(std::move(virtio_queue_buffer_pool_vmo)),
      virtio_queue_buffer_pool_pin_(std::move(virtio_queue_buffer_pool_pin)),
      virtio_queue_buffer_pool_physical_address_(virtio_queue_buffer_pool_physical_address),
      virtio_queue_buffer_pool_(virtio_queue_buffer_pool) {}

GpuDevice::~GpuDevice() {
  if (!virtio_queue_buffer_pool_.empty()) {
    zx_vaddr_t virtio_queue_buffer_pool_begin =
        reinterpret_cast<zx_vaddr_t>(virtio_queue_buffer_pool_.data());
    zx::vmar::root_self()->unmap(virtio_queue_buffer_pool_begin, virtio_queue_buffer_pool_.size());
  }
}

// static
zx::result<std::unique_ptr<GpuDevice>> GpuDevice::Create(
    fidl::ClientEnd<fuchsia_hardware_pci::Device> client_end) {
  zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> bti_and_backend_result =
      virtio::GetBtiAndBackend(ddk::Pci(std::move(client_end)));
  if (!bti_and_backend_result.is_ok()) {
    FDF_LOG(ERROR, "GetBtiAndBackend failed: %s", bti_and_backend_result.status_string());
    return bti_and_backend_result.take_error();
  }
  auto& [bti, backend] = bti_and_backend_result.value();

  zx::vmo virtio_queue_buffer_pool_vmo;
  zx_status_t status =
      zx::vmo::create(zx_system_get_page_size(), /*options=*/0, &virtio_queue_buffer_pool_vmo);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to allocate virtqueue buffers VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  uint64_t virtio_queue_buffer_pool_size;
  status = virtio_queue_buffer_pool_vmo.get_size(&virtio_queue_buffer_pool_size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to get virtqueue buffers VMO size: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // Commit backing store and get the physical address.
  zx_paddr_t virtio_queue_buffer_pool_physical_address;
  zx::pmt virtio_queue_buffer_pool_pin;
  status = bti.pin(ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, virtio_queue_buffer_pool_vmo, /*offset=*/0,
                   virtio_queue_buffer_pool_size, &virtio_queue_buffer_pool_physical_address, 1,
                   &virtio_queue_buffer_pool_pin);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to pin virtqueue buffers VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx_vaddr_t virtio_queue_buffer_pool_begin;
  status = zx::vmar::root_self()->map(
      ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, /*vmar_offset=*/0, virtio_queue_buffer_pool_vmo,
      /*vmo_offset=*/0, virtio_queue_buffer_pool_size, &virtio_queue_buffer_pool_begin);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map virtqueue buffers VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  FDF_LOG(INFO,
          "Allocated virtqueue buffers at virtual address 0x%016" PRIx64
          ", physical address 0x%016" PRIx64,
          virtio_queue_buffer_pool_begin, virtio_queue_buffer_pool_physical_address);

  // NOLINTBEGIN(performance-no-int-to-ptr): Casting from zx_vaddr_t to a
  // pointer is unavoidable due to the zx::vmar::map() API.
  cpp20::span<uint8_t> virtio_queue_buffer_pool(
      reinterpret_cast<uint8_t*>(virtio_queue_buffer_pool_begin), virtio_queue_buffer_pool_size);
  // NOLINTEND(performance-no-int-to-ptr)

  fbl::AllocChecker alloc_checker;
  auto device = fbl::make_unique_checked<GpuDevice>(
      &alloc_checker, std::move(bti), std::move(backend), std::move(virtio_queue_buffer_pool_vmo),
      std::move(virtio_queue_buffer_pool_pin), virtio_queue_buffer_pool_physical_address,
      virtio_queue_buffer_pool);
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for GpuDevice");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  status = device->Init();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize device");
    return zx::error(status);
  }

  return zx::ok(std::move(device));
}

zx_status_t GpuDevice::Init() {
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

  // Allocate the control virtqueue.
  status = virtio_queue_.Init(0, 16);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to allocate control virtqueue: %s", zx_status_get_string(status));
    return status;
  }

  StartIrqThread();
  DriverStatusOk();

  return ZX_OK;
}

void GpuDevice::IrqRingUpdate() {
  FDF_LOG(TRACE, "IrqRingUpdate()");

  // TODO(costan): `vring` may be used across threads without synchronization.
  //
  // VIRTIO spec Section 2.7.7.1 "Driver Requirements: Used Buffer Notification
  // Suppression" requires that drivers handle spurious notifications. A data
  // race can occur if a spurious interrupt is processed as the same time as an
  // ExchangeRequestResponse() method call.

  // Parse our descriptor chain, add back to the free queue
  auto free_chain = [this](vring_used_elem* used_elem) {
    uint16_t i = static_cast<uint16_t>(used_elem->id);
    struct vring_desc* desc = virtio_queue_.DescFromIndex(i);
    for (;;) {
      int next;

      if (desc->flags & VRING_DESC_F_NEXT) {
        next = desc->next;
      } else {
        // End of chain
        next = -1;
      }

      virtio_queue_.FreeDesc(i);

      if (next < 0) {
        break;
      }
      i = static_cast<uint16_t>(next);
      desc = virtio_queue_.DescFromIndex(i);
    }

    // TODO(costan): The signal may be fired prematurely.
    //
    // VIRTIO spec Section 2.7.7.1 "Driver Requirements: Used Buffer
    // Notification Suppression" requires that drivers handle spurious
    // notifications. This signal must only be fired when the virtqueue
    // allocated in ExchangeRequestResponse() is used by the device.

    // Notify the request thread
    fbl::AutoLock virtqueue_lock(&virtio_queue_mutex_);
    virtio_queue_buffer_used_signal_.Signal();
  };

  // Tell the ring to find free chains and hand it back to our lambda
  virtio_queue_.IrqRingUpdate(free_chain);
}

void GpuDevice::IrqConfigChange() { FDF_LOG(TRACE, "IrqConfigChange()"); }

}  // namespace virtio_display
