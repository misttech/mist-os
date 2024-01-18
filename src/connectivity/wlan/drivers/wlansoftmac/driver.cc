// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>

#include <memory>

#include <wlan/drivers/log.h>
#include <wlan/drivers/log_instance.h>

#include "softmac_binding.h"

namespace wlan::drivers::wlansoftmac {

static constexpr zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* device) -> zx_status_t {
      WLAN_LAMBDA_TRACE_DURATION("zx_drivers_ops_t.bind");
      wlan::drivers::log::Instance::Init(0);
      linfo("Binding wlansoftmac driver.");

      auto result = SoftmacBinding::New(device);
      if (result.is_error()) {
        auto status = result.error_value();
        lerror("Failed to bind: %d\n", status);
        return status;
      }

      // The release hook specified by zx_protocol_device will free this memory.
      [[maybe_unused]] auto _ = result.value().release();
      return ZX_OK;
    },
};

}  // namespace wlan::drivers::wlansoftmac

ZIRCON_DRIVER(wlan, wlan::drivers::wlansoftmac::driver_ops, "zircon", "0.1");
