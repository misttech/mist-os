// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8mmevk.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <fbl/alloc_checker.h>

#include "src/devices/board/drivers/imx8mmevk/imx8mmevk-bind.h"

namespace imx8mm_evk {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Imx8mmEvk::Create(void* ctx, zx_device_t* parent) {
  auto endpoints = fdf::CreateEndpoints<fpbus::PlatformBus>();
  if (endpoints.is_error()) {
    return endpoints.error_value();
  }

  zx_status_t status = device_connect_runtime_protocol(
      parent, fpbus::Service::PlatformBus::ServiceName, fpbus::Service::PlatformBus::Name,
      endpoints->server.TakeHandle().release());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to connect to platform bus: %s", zx_status_get_string(status));
    return status;
  }

  fdf::WireSyncClient<fpbus::PlatformBus> pbus(std::move(endpoints->client));
  auto result = pbus.buffer(fdf::Arena('INFO'))->GetBoardInfo();
  if (!result.ok()) {
    zxlogf(ERROR, "GetBoardInfo request failed: %s", result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "GetBoardInfo failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  fbl::AllocChecker ac;
  auto board = fbl::make_unique_checked<Imx8mmEvk>(&ac, parent, pbus.TakeClientEnd(),
                                                   fidl::ToNatural(result->value()->info));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  status = board->DdkAdd("imx8mmevk", DEVICE_ADD_NON_BINDABLE);
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed %s", zx_status_get_string(status));
    return status;
  }

  status = board->Start();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Board start failed %s", zx_status_get_string(status));
    return status;
  }

  [[maybe_unused]] auto* dummy = board.release();
  return ZX_OK;
}

zx_status_t Imx8mmEvk::Start() {
  auto cb = [](void* arg) -> int { return reinterpret_cast<Imx8mmEvk*>(arg)->Thread(); };
  int rc = thrd_create_with_name(&thread_, cb, this, "imx8mmevk-start-thread");
  return thrd_status_to_zx_status(rc);
}

int Imx8mmEvk::Thread() {
  zxlogf(DEBUG, "Imx8mmEvk thread cb called");

  auto status = GpioInit();
  if (status != ZX_OK) {
    zxlogf(ERROR, "GpioInit() failed: %s", zx_status_get_string(status));
    return thrd_error;
  }

  status = I2cInit();
  if (status != ZX_OK) {
    zxlogf(ERROR, "I2cInit() failed: %s", zx_status_get_string(status));
    return thrd_error;
  }

  return status;
}
}  // namespace imx8mm_evk

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = imx8mm_evk::Imx8mmEvk::Create;
  return ops;
}();

ZIRCON_DRIVER(imx8mmevk, driver_ops, "zircon", "0.1");
