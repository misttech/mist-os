// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>

#include <memory>

#include <wlan/drivers/log.h>
#include <wlan/drivers/log_instance.h>

#include "device.h"

zx_status_t wlan_bind(void* ctx, zx_device_t* device) {
  wlan::drivers::log::Instance::Init(0);
  linfo("Binding wlansoftmac driver.");

  auto wlandev = std::make_unique<wlan::Device>(device);
  auto status = wlandev->Bind();
  if (status != ZX_OK) {
    lerror("Failed to bind: %d\n", status);
    return status;
  }
  // devhost is now responsible for the memory used by wlandev. It will be
  // cleaned up in the Device::EthRelease() method.
  wlandev.release();
  return ZX_OK;
}

static constexpr zx_driver_ops_t wlan_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = wlan_bind;
  return ops;
}();

ZIRCON_DRIVER(wlan, wlan_driver_ops, "zircon", "0.1");
