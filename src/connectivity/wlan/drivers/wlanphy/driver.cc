// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "driver.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <zircon/status.h>

#include <mutex>

#include <wlan/drivers/log_instance.h>

#include "debug.h"
#include "device.h"

// Not guarded by a mutex, because it will be valid between .init and .release and nothing else will
// exist outside those two calls.
static async::Loop* loop = nullptr;
static std::once_flag flag;

zx_status_t wlanphy_init_loop() {
  zx_status_t status = ZX_OK;
  loop = new async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  status = loop->StartThread("wlanphy-loop");
  if (status != ZX_OK) {
    lerror("could not create event loop: %s", zx_status_get_string(status));
    delete loop;
    loop = nullptr;
    return status;
  } else {
    linfo("event loop started");
  }

  return status;
}

zx_status_t wlanphy_init(void** out_ctx) {
  static zx_status_t status = ZX_ERR_BAD_STATE;
  std::call_once(flag, []() { status = wlanphy_init_loop(); });
  return status;
}

zx_status_t wlanphy_bind(void* ctx, zx_device_t* device) {
  wlan::drivers::log::Instance::Init(kFiltSetting);
  ltrace_fn();
  zx_status_t status;

  auto wlanphy_dev = std::make_unique<wlanphy::Device>(device);

  if ((status = wlanphy_dev->DeviceAdd()) != ZX_OK) {
    lerror("failed adding wlanphy device: %s", zx_status_get_string(status));
  }

  if ((status = wlanphy_dev->ConnectToWlanPhyImpl())) {
    lerror("failed connecting to wlanphyimpl device: %s", zx_status_get_string(status));
  }

  if (status != ZX_OK) {
    lerror("could not bind: %s", zx_status_get_string(status));
  } else {
    // devhost is now responsible for the memory used by wlandev. It will be
    // cleaned up in the Device::Release() method.
    wlanphy_dev.release();
  }
  return status;
}

async_dispatcher_t* wlanphy_async_t() {
  if (loop == nullptr) {
    lerror("Loop is not initialized.");
    return nullptr;
  }

  return loop->dispatcher();
}

void wlanphy_destroy_loop() {
  loop->Shutdown();
  delete loop;
}

static constexpr zx_driver_ops_t wlanphy_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.init = wlanphy_init;
  ops.bind = wlanphy_bind;
  return ops;
}();

// clang-format: off
ZIRCON_DRIVER(wlan, wlanphy_driver_ops, "zircon", "0.1");
