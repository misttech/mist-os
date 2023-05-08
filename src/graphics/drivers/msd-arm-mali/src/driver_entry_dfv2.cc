// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpu.mali/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/lifecycle.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/fit/thread_safety.h>

#include "magma_util/short_macros.h"
#include "parent_device_dfv2.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_logger_dfv2.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_status.h"
#include "src/graphics/lib/magma/src/sys_driver_cpp/magma_driver_base.h"
#include "sys_driver_cpp/magma_driver.h"

class MaliDriver : public MagmaDriverBase {
 public:
  MaliDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaDriverBase("mali", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> MagmaStart() override {
    parent_device_ = ParentDeviceDFv2::Create(context().incoming());
    if (!parent_device_) {
      MAGMA_LOG(ERROR, "Failed to create ParentDeviceDFv2");
      return zx::error(ZX_ERR_INTERNAL);
    }

    std::lock_guard lock(magma_mutex());

    set_magma_driver(MagmaDriver::Create());
    if (!magma_driver()) {
      DMESSAGE("Failed to create MagmaDriver");
      return zx::error(ZX_ERR_INTERNAL);
    }

    // TODO(fxbug.dev/126333): Run in-driver tests.
    set_magma_system_device(magma_driver()->CreateDevice(parent_device_->ToDeviceHandle()));
    if (!magma_system_device()) {
      DMESSAGE("Failed to create MagmaSystemDevice");
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }

 private:
  std::unique_ptr<ParentDeviceDFv2> parent_device_;
};

using lifecycle = fdf::Lifecycle<MaliDriver>;
FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(lifecycle);
