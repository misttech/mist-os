// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>

namespace audio::aml_g12 {

static zx_status_t composite_bind(void* ctx, zx_device_t* device) { return ZX_ERR_NOT_SUPPORTED; }

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = composite_bind;
  return ops;
}();

}  // namespace audio::aml_g12

// clang-format off
ZIRCON_DRIVER(aml_g12_composite, audio::aml_g12::driver_ops, "aml-g12-aud-com", "0.1");
// clang-format on
