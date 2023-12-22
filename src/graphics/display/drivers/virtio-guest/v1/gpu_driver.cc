// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/virtio/backends/backend.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <zircon/status.h>

#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/virtio-guest/v1/gpu.h"

namespace virtio_display {

namespace {

zx_status_t VirtioDisplayBind(void* ctx, zx_device_t* parent) {
  char flag[32];
  zx_status_t status =
      device_get_variable(parent, "driver.virtio-gpu.disable", flag, sizeof(flag), nullptr);
  // If gpu disabled:
  if (status == ZX_OK && (!strcmp(flag, "1") || !strcmp(flag, "true") || !strcmp(flag, "on"))) {
    zxlogf(INFO, "driver.virtio-gpu.disabled=1, not binding to the GPU");
    return ZX_ERR_NOT_FOUND;
  }

  zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> bti_and_backend_result =
      virtio::GetBtiAndBackend(parent);
  if (!bti_and_backend_result.is_ok()) {
    zxlogf(ERROR, "GetBtiAndBackend failed: %s", bti_and_backend_result.status_string());
    return bti_and_backend_result.error_value();
  }
  auto& [bti, backend] = bti_and_backend_result.value();

  fbl::AllocChecker alloc_checker;
  auto device = fbl::make_unique_checked<GpuDevice>(&alloc_checker, parent, std::move(bti),
                                                    std::move(backend));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for GpuDevice");
    return ZX_ERR_NO_MEMORY;
  }

  status = device->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize device: %s", zx_status_get_string(status));
    return status;
  }

  // devmgr now owns the memory for `device`.
  [[maybe_unused]] auto device_ptr = device.release();
  return ZX_OK;
}

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = VirtioDisplayBind,
};

}  // namespace

}  // namespace virtio_display

ZIRCON_DRIVER(virtio_gpu, virtio_display::kDriverOps, "zircon", "0.1");
