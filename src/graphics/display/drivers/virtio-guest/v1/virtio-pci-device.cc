// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/virtio-pci-device.h"

#include <lib/ddk/debug.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/bti.h>
#include <lib/zx/pmt.h>
#include <lib/zx/result.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <memory>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/virtio-guest/v1/virtio-abi.h"

namespace virtio_display {

// static
zx::result<std::unique_ptr<VirtioPciDevice>> VirtioPciDevice::Create(
    zx::bti bti, std::unique_ptr<virtio::Backend> backend) {
  zx::vmo virtio_queue_buffer_pool_vmo;
  zx_status_t status =
      zx::vmo::create(zx_system_get_page_size(), /*options=*/0, &virtio_queue_buffer_pool_vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate virtqueue buffers VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  uint64_t virtio_queue_buffer_pool_size;
  status = virtio_queue_buffer_pool_vmo.get_size(&virtio_queue_buffer_pool_size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get virtqueue buffers VMO size: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // Commit backing store and get the physical address.
  zx_paddr_t virtio_queue_buffer_pool_physical_address;
  zx::pmt virtio_queue_buffer_pool_pin;
  status = bti.pin(ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, virtio_queue_buffer_pool_vmo, /*offset=*/0,
                   virtio_queue_buffer_pool_size, &virtio_queue_buffer_pool_physical_address, 1,
                   &virtio_queue_buffer_pool_pin);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to pin virtqueue buffers VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx_vaddr_t virtio_queue_buffer_pool_begin;
  status = zx::vmar::root_self()->map(
      ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, /*vmar_offset=*/0, virtio_queue_buffer_pool_vmo,
      /*vmo_offset=*/0, virtio_queue_buffer_pool_size, &virtio_queue_buffer_pool_begin);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map virtqueue buffers VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zxlogf(INFO,
         "Allocated virtqueue buffers at virtual address 0x%016" PRIx64
         ", physical address 0x%016" PRIx64,
         virtio_queue_buffer_pool_begin, virtio_queue_buffer_pool_physical_address);

  // NOLINTBEGIN(performance-no-int-to-ptr): Casting from zx_vaddr_t to a
  // pointer is unavoidable due to the zx::vmar::map() API.
  cpp20::span<uint8_t> virtio_queue_buffer_pool(
      reinterpret_cast<uint8_t*>(virtio_queue_buffer_pool_begin), virtio_queue_buffer_pool_size);
  // NOLINTEND(performance-no-int-to-ptr)

  fbl::AllocChecker alloc_checker;
  auto virtio_device = fbl::make_unique_checked<VirtioPciDevice>(
      &alloc_checker, std::move(bti), std::move(backend), std::move(virtio_queue_buffer_pool_vmo),
      std::move(virtio_queue_buffer_pool_pin), virtio_queue_buffer_pool_physical_address,
      virtio_queue_buffer_pool);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for GpuDevice");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  status = virtio_device->Init();
  if (status != ZX_OK) {
    // VirtioPciDevice::Init() logs on error.
    return zx::error_result(status);
  }
  return zx::ok(std::move(virtio_device));
}

VirtioPciDevice::VirtioPciDevice(zx::bti bti, std::unique_ptr<virtio::Backend> backend,
                                 zx::vmo virtio_queue_buffer_pool_vmo,
                                 zx::pmt virtio_queue_buffer_pool_pin,
                                 zx_paddr_t virtio_queue_buffer_pool_physical_address,
                                 cpp20::span<uint8_t> virtio_queue_buffer_pool)
    : virtio::Device(std::move(bti), std::move(backend)),
      virtio_queue_(this),
      virtio_queue_buffer_pool_vmo_(std::move(virtio_queue_buffer_pool_vmo)),
      virtio_queue_buffer_pool_pin_(std::move(virtio_queue_buffer_pool_pin)),
      virtio_queue_buffer_pool_physical_address_(virtio_queue_buffer_pool_physical_address),
      virtio_queue_buffer_pool_(virtio_queue_buffer_pool) {}

VirtioPciDevice::~VirtioPciDevice() {
  Release();

  if (!virtio_queue_buffer_pool_.empty()) {
    zx_vaddr_t virtio_queue_buffer_pool_begin =
        reinterpret_cast<zx_vaddr_t>(virtio_queue_buffer_pool_.data());
    zx::vmar::root_self()->unmap(virtio_queue_buffer_pool_begin, virtio_queue_buffer_pool_.size());
  }
}

zx_status_t VirtioPciDevice::Init() {
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

  zx_status_t status = DeviceStatusFeaturesOk();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Feature negotiation failed: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate the control virtqueue.
  fbl::AutoLock virtio_queue_lock(&virtio_queue_mutex_);
  status = virtio_queue_.Init(0, 16);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate vring: %s", zx_status_get_string(status));
    return status;
  }

  StartIrqThread();
  DriverStatusOk();

  return ZX_OK;
}

void VirtioPciDevice::IrqRingUpdate() {
  zxlogf(TRACE, "IrqRingUpdate()");

  // The lambda passed to virtio::Ring::IrqRingUpdate() should be annotated
  // __TA_REQUIRES(virtio_queue_mutex_). However, Clang's Thread Safety analysis
  // cannot prove the correctness of this annotation.
  //
  // Clang's static analyzer does not do inter-procedural analysis such as
  // inlining. Therefore, the analyzer does not know that IrqRingUpdate() only
  // calls the lambda argument before returning. So, it does not know that the
  // fbl::AutoLock is alive during the lambda calls, which would satisfy the
  // __TA_REQUIRES() annotation on the lambda.
  fbl::AutoLock virtio_queue_lock(&virtio_queue_mutex_);
  virtio_queue_.IrqRingUpdate([&](vring_used_elem* used_buffer_info)
                                  __TA_NO_THREAD_SAFETY_ANALYSIS {
                                    const uint32_t used_descriptor_index = used_buffer_info->id;
                                    VirtioBufferUsedByDevice(used_descriptor_index);
                                  });
}

void VirtioPciDevice::IrqConfigChange() { zxlogf(TRACE, "IrqConfigChange()"); }

const char* VirtioPciDevice::tag() const { return "virtio-gpu"; }

void VirtioPciDevice::VirtioBufferUsedByDevice(uint32_t used_descriptor_index) {
  if (unlikely(used_descriptor_index > std::numeric_limits<uint16_t>::max())) {
    zxlogf(WARNING, "GPU device reported invalid used descriptor index: %" PRIu32,
           used_descriptor_index);
    return;
  }
  // The check above ensures that the static_cast below does not lose any
  // information.
  uint16_t used_descriptor_index_u16 = static_cast<uint16_t>(used_descriptor_index);

  while (true) {
    struct vring_desc* buffer_descriptor = virtio_queue_.DescFromIndex(used_descriptor_index_u16);

    // Read information before the descriptor is freed.
    const bool next_descriptor_index_is_valid = (buffer_descriptor->flags & VRING_DESC_F_NEXT) != 0;
    const uint16_t next_descriptor_index = buffer_descriptor->next;
    virtio_queue_.FreeDesc(used_descriptor_index_u16);

    // VIRTIO spec Section 2.7.7.1 "Driver Requirements: Used Buffer
    // Notification Suppression" requires that drivers handle spurious
    // notifications. So, we only notify the request thread when the device
    // reports having used the specific buffer that populated the request.
    // buffer has been used by the device.
    if (virtio_queue_request_index_.has_value() &&
        virtio_queue_request_index_.value() == used_descriptor_index) {
      virtio_queue_request_index_.reset();

      // TODO(costan): Ideally, the variable would be signaled when
      // `virtio_queue_mutex_` is not locked. This would avoid having the
      // ExchangeRequestResponse() thread wake up, only to have to wait on the
      // mutex. Some options are:
      // * Plumb a (currently 1-element long) list of variables to be signaled
      //   from VirtioBufferUsedByDevice() to IrqRingUpdate().
      // * Replace virtio::Ring::IrqRingUpdate() with an iterator abstraction.
      //   The list of variables to be signaled would not need to be plumbed
      //   across methods.
      virtio_queue_buffer_used_signal_.Signal();
    }

    if (!next_descriptor_index_is_valid) {
      break;
    }
    used_descriptor_index_u16 = next_descriptor_index;
  }
}

}  // namespace virtio_display
