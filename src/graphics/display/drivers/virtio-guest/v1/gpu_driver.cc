// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/driver.h>
#include <lib/virtio/driver_utils.h>

#include "src/graphics/display/drivers/virtio-guest/v1/gpu.h"

namespace virtio_display {

namespace {

zx_status_t VirtioDisplayBind(void* ctx, zx_device_t* bus_device) {
  char flag[32];
  zx_status_t status =
      device_get_variable(bus_device, "driver.virtio-gpu.disable", flag, sizeof(flag), nullptr);
  // If gpu disabled:
  if (status == ZX_OK && (!strcmp(flag, "1") || !strcmp(flag, "true") || !strcmp(flag, "on"))) {
    zxlogf(INFO, "driver.virtio-gpu.disabled=1, not binding to the GPU");
    return ZX_ERR_NOT_FOUND;
  }
  return virtio::CreateAndBind<virtio_display::GpuDevice>(ctx, bus_device);
}

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = VirtioDisplayBind,
};

}  // namespace

}  // namespace virtio_display

ZIRCON_DRIVER(virtio_gpu, virtio_display::kDriverOps, "zircon", "0.1");
