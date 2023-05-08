// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "parent_device_dfv2.h"

#include <fidl/fuchsia.hardware.gpu.mali/cpp/driver/wire.h>

#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_interrupt.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_mmio.h"

// TODO(fxbug.dev/126333): Implement.
extern "C" zx_handle_t get_root_resource() { return 0; }

ParentDeviceDFv2::ParentDeviceDFv2(
    std::shared_ptr<fdf::Namespace> incoming,
    fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev)
    : incoming_(std::move(incoming)), pdev_(std::move(pdev)) {}

zx::bti ParentDeviceDFv2::GetBusTransactionInitiator() {
  auto res = pdev_->GetBti(0);
  if (!res.ok()) {
    DMESSAGE("failed to get bus transaction initiator: %s", res.status_string());
    return zx::bti();
  }
  if (!res->is_ok()) {
    DMESSAGE("failed to get bus transaction initiator: %d", res->error_value());
    return zx::bti();
  }
  return std::move(res.value()->bti);
}

std::unique_ptr<magma::PlatformMmio> ParentDeviceDFv2::CpuMapMmio(
    unsigned int index, magma::PlatformMmio::CachePolicy cache_policy) {
  auto res = pdev_->GetMmio(index);
  if (!res.ok()) {
    DMESSAGE("failed to get mmio: %s", res.status_string());
    return nullptr;
  }
  if (!res->is_ok()) {
    DMESSAGE("failed to get mmio: %d", res->error_value());
    return nullptr;
  }

  size_t offset = 0;
  size_t size = 0;
  zx::vmo vmo;
  if (res->value()->has_offset()) {
    offset = res->value()->offset();
  }
  if (res->value()->has_size()) {
    size = res->value()->size();
  }
  if (res->value()->has_vmo()) {
    vmo = std::move(res->value()->vmo());
  }

  auto mmio_buffer =
      fdf::MmioBuffer::Create(offset, size, std::move(vmo), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (!mmio_buffer.is_ok()) {
    DMESSAGE("Failed to make mmio buffer %s", mmio_buffer.status_string());
    return nullptr;
  }

  std::unique_ptr<magma::ZirconPlatformMmio> mmio(
      new magma::ZirconPlatformMmio(mmio_buffer.value().release()));
  return mmio;
}

std::unique_ptr<magma::PlatformInterrupt> ParentDeviceDFv2::RegisterInterrupt(unsigned int index) {
  auto res = pdev_->GetInterrupt(index, 0);
  if (!res.ok()) {
    DMESSAGE("failed to register interrupt: %s", res.status_string());
    return nullptr;
  }
  if (!res->is_ok()) {
    DMESSAGE("failed to register interrupt: %d", res->error_value());
    return nullptr;
  }

  return std::make_unique<magma::ZirconPlatformInterrupt>(zx::handle(std::move(res->value()->irq)));
}

zx::result<fdf::ClientEnd<fuchsia_hardware_gpu_mali::ArmMali>>
ParentDeviceDFv2::ConnectToMaliRuntimeProtocol() {
  auto mali_protocol = incoming_->Connect<fuchsia_hardware_gpu_mali::Service::ArmMali>("mali");
  if (mali_protocol.is_error()) {
    DMESSAGE("Error requesting mali protocol: %s", mali_protocol.status_string());
  }
  return mali_protocol;
}

// static
std::unique_ptr<ParentDeviceDFv2> ParentDeviceDFv2::Create(
    std::shared_ptr<fdf::Namespace> incoming) {
  auto platform_device =
      incoming->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
  if (!platform_device.is_ok()) {
    return DRETP(nullptr, "Error requesting platform device service: %s",
                 platform_device.status_string());
  }
  return std::make_unique<ParentDeviceDFv2>(std::move(incoming),
                                            fidl::WireSyncClient(std::move(*platform_device)));
}
