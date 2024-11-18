// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon_platform_device.h"

#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/magma/platform/platform_mmio.h>
#include <lib/magma/util/dlog.h>
#include <lib/magma/util/short_macros.h>
#include <lib/zx/bti.h>
#include <lib/zx/vmo.h>
#include <zircon/process.h>

#include "zircon_platform_handle.h"
#include "zircon_platform_interrupt.h"
#include "zircon_platform_mmio.h"

namespace magma {

std::unique_ptr<PlatformMmio> ZirconPlatformDevice::CpuMapMmio(
    unsigned int index, PlatformMmio::CachePolicy cache_policy) {
  DLOG("CpuMapMmio index %d", index);

  zx_status_t status;

  std::optional<fdf::MmioBuffer> mmio_buffer;
  status = pdev_.MapMmio(index, &mmio_buffer);
  if (status != ZX_OK) {
    DRETP(nullptr, "mapping resource failed");
  }

  DLOG("map_mmio index %d cache_policy %d returned: 0x%x", index, static_cast<int>(cache_policy),
       mmio_buffer->get_vmo()->get());

  std::unique_ptr<ZirconPlatformMmio> mmio(new ZirconPlatformMmio(std::move(mmio_buffer.value())));

  zx::bti bti;
  status = pdev_.GetBti(0, &bti);
  if (status != ZX_OK)
    return DRETP(nullptr, "failed to get bus transaction initiator for pinning mmio: %d", status);

  if (!mmio->Pin(bti))
    return DRETP(nullptr, "Failed to pin mmio");

  return mmio;
}

std::unique_ptr<PlatformBuffer> ZirconPlatformDevice::GetMmioBuffer(unsigned int index) {
  pdev_mmio_t mmio;
  zx_status_t status = pdev_.GetMmio(index, &mmio);
  if (status != ZX_OK)
    return DRETP(nullptr, "pdev_get_mmio failed: %d", status);

  return magma::PlatformBuffer::Import(mmio.vmo);
}

std::unique_ptr<PlatformInterrupt> ZirconPlatformDevice::RegisterInterrupt(unsigned int index) {
  zx::interrupt interrupt;
  zx_status_t status = pdev_.GetInterrupt(index, 0, &interrupt);
  if (status != ZX_OK)
    return DRETP(nullptr, "register interrupt failed");

  return std::make_unique<ZirconPlatformInterrupt>(zx::handle(interrupt.release()));
}

std::unique_ptr<PlatformHandle> ZirconPlatformDevice::GetBusTransactionInitiator() const {
  zx::bti bti;
  zx_status_t status = pdev_.GetBti(0, &bti);
  if (status != ZX_OK)
    return DRETP(nullptr, "failed to get bus transaction initiator");

  return std::make_unique<ZirconPlatformHandle>(zx::handle(bti.release()));
}

std::unique_ptr<PlatformDevice> PlatformDevice::Create(void* device_handle) {
  if (!device_handle)
    return DRETP(nullptr, "device_handle is null, cannot create PlatformDevice");

  zx_device_t* zx_device = static_cast<zx_device_t*>(device_handle);

  pdev_protocol_t pdev;
  zx_status_t status = device_get_protocol(zx_device, ZX_PROTOCOL_PDEV, &pdev);
  if (status != ZX_OK) {
    // if ZX_PROTOCOL_PDEV is not available, try using find via fragment.
    status = device_get_fragment_protocol(zx_device, "pdev", ZX_PROTOCOL_PDEV, &pdev);
  }

  if (status == ZX_ERR_NOT_SUPPORTED) {
    return std::make_unique<ZirconPlatformDeviceWithoutProtocol>(zx_device);
  }

  if (status == ZX_OK) {
    pdev_device_info_t device_info;
    zx_status_t status = pdev_get_device_info(&pdev, &device_info);
    if (status != ZX_OK)
      return DRETP(nullptr, "pdev_get_device_info failed: %d", status);

    return std::make_unique<ZirconPlatformDevice>(zx_device, pdev, device_info.mmio_count);
  }

  return DRETP(nullptr, "Error requesting protocol: %d", status);
}

}  // namespace magma
