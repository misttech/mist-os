// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v2/gpu-device.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/pmt.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

namespace virtio_display {

GpuDevice::GpuDevice(zx::vmo virtio_queue_buffer_pool_vmo, zx::pmt virtio_queue_buffer_pool_pin,
                     zx_paddr_t virtio_queue_buffer_pool_physical_address,
                     cpp20::span<uint8_t> virtio_queue_buffer_pool)
    : virtio_queue_buffer_pool_vmo_(std::move(virtio_queue_buffer_pool_vmo)),
      virtio_queue_buffer_pool_pin_(std::move(virtio_queue_buffer_pool_pin)),
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
  zx_paddr_t virtio_queue_buffer_pool_physical_address = 0;
  zx::pmt virtio_queue_buffer_pool_pin;

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
      &alloc_checker, std::move(virtio_queue_buffer_pool_vmo),
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

void GpuDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_display_engine::Engine> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(WARNING, "Received an unknown method with ordinal %lu", metadata.method_ordinal);
}

zx_status_t GpuDevice::Init() { return ZX_OK; }

}  // namespace virtio_display
