// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>

#include "simple-display.h"

namespace simple_display {

namespace {

zx_status_t BindIntelDisplay(void* ctx, zx_device_t* dev) {
  return bind_simple_pci_display_bootloader(dev, "intel", 2u);
}

constexpr zx_driver_ops_t kIntelDisplayDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = BindIntelDisplay,
};

}  // namespace

}  // namespace simple_display

ZIRCON_DRIVER(intel_disp, simple_display::kIntelDisplayDriverOps, "zircon", "0.1");
