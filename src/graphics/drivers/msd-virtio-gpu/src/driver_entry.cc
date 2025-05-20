// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.virtio/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/magma/platform/platform_bus_mapper.h>
#include <lib/magma_service/msd.h>
#include <lib/magma_service/sys_driver/magma_driver_base.h>

#include "virtio_gpu_control.h"

class VirtioDriver : public msd::MagmaDriverBase, VirtioGpuControlFidl {
 public:
  explicit VirtioDriver(fdf::DriverStartArgs start_args,
                        fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : msd::MagmaDriverBase("virtio", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> MagmaStart() override;

  void Stop() override { FDF_LOG(INFO, "VirtioDevice::Stop"); }
};

zx::result<> VirtioDriver::MagmaStart() {
  FDF_LOG(INFO, "VirtioDevice::Start");

  zx::result info_resource = GetInfoResource();
  // Info resource may not be available on user builds.
  if (info_resource.is_ok()) {
    magma::PlatformBusMapper::SetInfoResource(std::move(*info_resource));
  }

  auto result = VirtioGpuControlFidl::Init(incoming());
  if (result.is_error()) {
    FDF_LOG(ERROR, "VirtioGpuControlFidl::Init failed: %s", result.status_string());
    return result.take_error();
  }

  {
    std::lock_guard lock(magma_mutex());

    set_magma_driver(msd::Driver::Create());

    if (!magma_driver()) {
      FDF_LOG(ERROR, "msd::Driver::Create failed");
      return zx::error(ZX_ERR_INTERNAL);
    }

    auto msd_device = magma_driver()->CreateDevice(static_cast<VirtioGpuControl*>(this));

    set_magma_system_device(msd::MagmaSystemDevice::Create(magma_driver(), std::move(msd_device)));

    if (!magma_system_device()) {
      FDF_LOG(ERROR, "msd::MagmaSystemDevice::Create failed");
      return zx::error(ZX_ERR_INTERNAL);
    }
  }

  return zx::ok();
}

FUCHSIA_DRIVER_EXPORT(VirtioDriver);
