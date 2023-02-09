// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "parent_device.h"

#include <lib/ddk/driver.h>
#include <lib/device-protocol/platform-device.h>

#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_interrupt.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_mmio.h"

msd::DeviceHandle* ZxDeviceToDeviceHandle(zx_device_t* device) {
  return reinterpret_cast<msd::DeviceHandle*>(device);
}

zx_device_t* ParentDevice::GetDeviceHandle() { return parent_; }

bool ParentDevice::GetProtocol(uint32_t proto_id, void* proto_out) {
  zx_status_t status = device_get_protocol(parent_, proto_id, proto_out);
  if (status != ZX_OK) {
    return DRETF(false, "device_get_protocol for %d failed: %d", proto_id, status);
  }
  return true;
}

zx::bti ParentDevice::GetBusTransactionInitiator() const {
  zx_handle_t bti_handle;
  zx_status_t status = pdev_get_bti(&pdev_, 0, &bti_handle);
  if (status != ZX_OK) {
    DMESSAGE("failed to get bus transaction initiator");
    return zx::bti();
  }

  return zx::bti(bti_handle);
}

std::unique_ptr<magma::PlatformMmio> ParentDevice::CpuMapMmio(
    unsigned int index, magma::PlatformMmio::CachePolicy cache_policy) {
  DLOG("CpuMapMmio index %d", index);

  zx_status_t status;
  mmio_buffer_t mmio_buffer;

  status = pdev_map_mmio_buffer(&pdev_, index, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio_buffer);
  if (status != ZX_OK) {
    DRETP(nullptr, "mapping resource failed");
  }

  std::unique_ptr<magma::ZirconPlatformMmio> mmio(new magma::ZirconPlatformMmio(mmio_buffer));

  DLOG("map_mmio index %d cache_policy %d returned: 0x%x", index, static_cast<int>(cache_policy),
       mmio_buffer.vmo);

  zx::bti bti_handle;
  status = pdev_get_bti(&pdev_, 0, bti_handle.reset_and_get_address());
  if (status != ZX_OK)
    return DRETP(nullptr, "failed to get bus transaction initiator for pinning mmio: %d", status);

  if (!mmio->Pin(bti_handle.get()))
    return DRETP(nullptr, "Failed to pin mmio");

  return mmio;
}

std::unique_ptr<magma::PlatformInterrupt> ParentDevice::RegisterInterrupt(unsigned int index) {
  zx_handle_t interrupt_handle;
  zx_status_t status = pdev_get_interrupt(&pdev_, index, 0, &interrupt_handle);
  if (status != ZX_OK)
    return DRETP(nullptr, "register interrupt failed");

  return std::make_unique<magma::ZirconPlatformInterrupt>(zx::handle(interrupt_handle));
}

// static
std::unique_ptr<ParentDevice> ParentDevice::Create(msd::DeviceHandle* device_handle) {
  if (!device_handle)
    return DRETP(nullptr, "device_handle is null, cannot create PlatformDevice");

  zx_device_t* zx_device = reinterpret_cast<zx_device_t*>(device_handle);

  pdev_protocol_t pdev;
  zx_status_t status = device_get_protocol(zx_device, ZX_PROTOCOL_PDEV, &pdev);
  if (status != ZX_OK) {
    return DRETP(nullptr, "Error requesting protocol: %d", status);
  }

  return std::make_unique<ParentDevice>(zx_device, pdev);
}
