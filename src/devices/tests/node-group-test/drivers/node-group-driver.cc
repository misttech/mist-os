// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/tests/node-group-test/drivers/node-group-driver.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>

namespace node_group_driver {

// static
zx_status_t NodeGroupDriver::Bind(void* ctx, zx_device_t* device) {
  auto dev = std::make_unique<NodeGroupDriver>(device);

  zx_status_t status = dev->DdkAdd("node_group");
  if (status != ZX_OK) {
    return status;
  }

  [[maybe_unused]] auto ptr = dev.release();
  return ZX_OK;
}

static zx_driver_ops_t kDriverOps = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = NodeGroupDriver::Bind;
  return ops;
}();

}  // namespace node_group_driver

ZIRCON_DRIVER(node_group_driver, node_group_driver::kDriverOps, "zircon", "0.1");
