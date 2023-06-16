// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "parent_device_dfv1.h"

#include <lib/ddk/driver.h>

#include "src/graphics/lib/magma/src/magma_util/platform/platform_thread.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_interrupt.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_mmio.h"

msd::DeviceHandle* ZxDeviceToDeviceHandle(zx_device_t* device) {
  return reinterpret_cast<msd::DeviceHandle*>(device);
}

bool ParentDeviceDFv1::SetThreadRole(const char* role_name) {
  return magma::PlatformThreadHelper::SetRole(parent_, role_name);
}

zx::bti ParentDeviceDFv1::GetBusTransactionInitiator() {
  zx::bti bti;
  zx_status_t status = pdev_.GetBti(0, &bti);
  if (status != ZX_OK) {
    DMESSAGE("failed to get bus transaction initiator");
    return zx::bti();
  }

  return bti;
}

std::unique_ptr<magma::PlatformMmio> ParentDeviceDFv1::CpuMapMmio(
    unsigned int index, magma::PlatformMmio::CachePolicy cache_policy) {
  DLOG("CpuMapMmio index %d", index);

  zx_status_t status;
  std::optional<fdf::MmioBuffer> mmio_buffer;
  status = pdev_.MapMmio(index, &mmio_buffer);
  if (status != ZX_OK) {
    DRETP(nullptr, "mapping resource failed");
  }

  DLOG("map_mmio index %d cache_policy %d returned: 0x%x", index, static_cast<int>(cache_policy),
       mmio_buffer.value().get_vmo()->get());

  std::unique_ptr<magma::ZirconPlatformMmio> mmio(
      new magma::ZirconPlatformMmio(std::move(mmio_buffer.value())));

  zx::bti bti_handle;
  status = pdev_.GetBti(0, &bti_handle);
  if (status != ZX_OK)
    return DRETP(nullptr, "failed to get bus transaction initiator for pinning mmio: %d", status);

  if (!mmio->Pin(bti_handle))
    return DRETP(nullptr, "Failed to pin mmio");

  return mmio;
}

std::unique_ptr<magma::PlatformInterrupt> ParentDeviceDFv1::RegisterInterrupt(unsigned int index) {
  zx::interrupt interrupt;
  zx_status_t status = pdev_.GetInterrupt(index, 0, &interrupt);
  if (status != ZX_OK)
    return DRETP(nullptr, "register interrupt failed");

  return std::make_unique<magma::ZirconPlatformInterrupt>(zx::handle(interrupt.release()));
}

zx::result<fdf::ClientEnd<fuchsia_hardware_gpu_mali::ArmMali>>
ParentDeviceDFv1::ConnectToMaliRuntimeProtocol() {
  auto endpoints =
      fdf::CreateEndpoints<fuchsia_hardware_gpu_mali::Service::ArmMali::ProtocolType>();
  if (!endpoints.is_ok()) {
    return endpoints.take_error();
  }
  zx_status_t status = device_connect_fragment_runtime_protocol(
      parent_, "mali", fuchsia_hardware_gpu_mali::Service::ArmMali::ServiceName,
      fuchsia_hardware_gpu_mali::Service::ArmMali::Name, endpoints->server.TakeChannel().release());

  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(endpoints->client));
}

// static
std::unique_ptr<ParentDeviceDFv1> ParentDeviceDFv1::Create(zx_device_t* zx_device) {
  if (!zx_device)
    return DRETP(nullptr, "device_handle is null, cannot create PlatformDevice");

  auto pdev = ddk::PDevFidl::Create(zx_device, "pdev");
  if (pdev.is_error()) {
    return DRETP(nullptr, "Error requesting pdev: %s", pdev.status_string());
  }

  return std::make_unique<ParentDeviceDFv1>(zx_device, std::move(*pdev));
}
