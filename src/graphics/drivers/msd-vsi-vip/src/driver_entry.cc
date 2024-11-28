// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fit/thread_safety.h>
#include <lib/magma/platform/platform_handle.h>
#include <lib/magma/platform/zircon/zircon_platform_device_dfv2.h>
#include <lib/magma/platform/zircon/zircon_platform_logger_dfv2.h>
#include <lib/magma/platform/zircon/zircon_platform_status.h>
#include <lib/magma/util/short_macros.h>
#include <lib/magma_service/sys_driver/magma_driver_base.h>
#include <lib/magma_service/sys_driver/magma_system_device.h>
#include <zircon/process.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <memory>

#include "parent_device_dfv2.h"

#if MAGMA_TEST_DRIVER
using MagmaDriverBaseType = msd::MagmaTestDriverBase;

zx_status_t magma_indriver_test(ParentDeviceDfv2* device);

#else
using MagmaDriverBaseType = msd::MagmaProductionDriverBase;
#endif

class NpuDevice : public MagmaDriverBaseType {
 public:
  NpuDevice(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaDriverBaseType("vsi-vip", std::move(start_args), std::move(driver_dispatcher)),
        parent_{.incoming_ = incoming()} {}

  zx::result<> MagmaStart() override;

 private:
  ParentDeviceDfv2 parent_;
};

zx::result<> NpuDevice::MagmaStart() {
  std::lock_guard lock(magma_mutex());
  set_magma_driver(msd::Driver::Create());
  if (!magma_driver()) {
    DMESSAGE("Failed to create MagmaDriver");
    return zx::error(ZX_ERR_INTERNAL);
  }

  set_magma_system_device(msd::MagmaSystemDevice::Create(
      magma_driver(),
      magma_driver()->CreateDevice(reinterpret_cast<msd::DeviceHandle*>(&parent_))));
  if (!magma_system_device()) {
    MAGMA_LOG(ERROR, "Failed to create device");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  return zx::ok();
}

FUCHSIA_DRIVER_EXPORT(NpuDevice);
