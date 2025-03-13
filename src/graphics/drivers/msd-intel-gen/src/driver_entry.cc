// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <fuchsia/hardware/intelgpucore/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/magma/platform/platform_bus_mapper.h>
#include <lib/magma/platform/zircon/zircon_platform_status.h>
#include <lib/magma/util/dlog.h>
#include <lib/magma/util/short_macros.h>
#include <lib/magma_service/msd_defs.h>
#include <lib/magma_service/sys_driver/magma_driver_base.h>
#include <lib/zx/channel.h>
#include <lib/zx/resource.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/process.h>
#include <zircon/types.h>

#if MAGMA_TEST_DRIVER
#include "msd_intel_pci_device.h"

zx_status_t magma_indriver_test(magma::PlatformPciDevice* platform_device);
using MagmaDriverBaseType = msd::MagmaTestDriverBase;
#else
using MagmaDriverBaseType = msd::MagmaProductionDriverBase;
#endif

class IntelDevice : public MagmaDriverBaseType {
 public:
  IntelDevice(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaDriverBaseType("magma_gpu", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> MagmaStart() override;

  void Stop() override {
    MagmaDriverBaseType::Stop();
    magma::PlatformBusMapper::SetInfoResource(zx::resource{});
  }

 private:
  intel_gpu_core_protocol_t gpu_core_protocol_;
};

zx::result<> IntelDevice::MagmaStart() {
  zx::result info_resource = GetInfoResource();
  // Info resource may not be available on user builds.
  if (info_resource.is_ok()) {
    magma::PlatformBusMapper::SetInfoResource(std::move(*info_resource));
  }

  std::lock_guard lock(magma_mutex());
  set_magma_driver(msd::Driver::Create());
  if (!magma_driver()) {
    DMESSAGE("Failed to create MagmaDriver");
    return zx::error(ZX_ERR_INTERNAL);
  }
  DLOG("Created device %p", magma_system_device());

  zx::result banjo = compat::ConnectBanjo<ddk::IntelGpuCoreProtocolClient>(incoming());
  if (banjo.is_error()) {
    MAGMA_LOG(ERROR, "Failed to connect to banjo %s", banjo.status_string());
    return banjo.take_error();
  }
  banjo->GetProto(&gpu_core_protocol_);

#if MAGMA_TEST_DRIVER
  DLOG("running magma indriver test");
  {
    auto platform_device = MsdIntelPciDevice::CreateShim(&gpu_core_protocol_);
    set_unit_test_status(magma_indriver_test(platform_device.get()));
  }
#endif

  set_magma_system_device(msd::MagmaSystemDevice::Create(
      magma_driver(),
      magma_driver()->CreateDevice(reinterpret_cast<msd::DeviceHandle*>(&gpu_core_protocol_))));
  if (!magma_system_device()) {
    MAGMA_LOG(ERROR, "Failed to create device");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  return zx::ok();
}

FUCHSIA_DRIVER_EXPORT(IntelDevice);
