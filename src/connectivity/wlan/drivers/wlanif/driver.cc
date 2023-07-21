// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <zircon/status.h>

#include <wlan/drivers/log_instance.h>

#include "debug.h"
#include "device.h"

zx_status_t wlan_fullmac_bind(void* ctx, zx_device_t* device) {
  wlan::drivers::log::Instance::Init(kFiltSetting);
  ltrace_fn();
  zx_status_t status;

  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImpl>();
  if (endpoints.is_error()) {
    lerror("Creating end point error: %s", zx_status_get_string(endpoints.status_value()));
    return endpoints.status_value();
  }

  auto wlan_fullmac_dev = std::make_unique<wlanif::Device>(device);

  if ((status = wlan_fullmac_dev->ConnectToWlanFullmacImpl()) != ZX_OK) {
    lerror("Failed connecting to wlan fullmac impl driver: %s", zx_status_get_string(status));
  }

  if ((status = wlan_fullmac_dev->Bind()) != ZX_OK) {
    lerror("Failed adding wlan fullmac device: %s", zx_status_get_string(status));
  }

  if (status != ZX_OK) {
    lerror("could not bind: %s", zx_status_get_string(status));
  } else {
    // devhost is now responsible for the memory used by wlandev. It will be
    // cleaned up in the Device::Release() method.
    wlan_fullmac_dev.release();
  }
  return status;
}

static constexpr zx_driver_ops_t wlan_fullmac_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = wlan_fullmac_bind;
  return ops;
}();

ZIRCON_DRIVER(wlan, wlan_fullmac_driver_ops, "zircon", "0.1");
