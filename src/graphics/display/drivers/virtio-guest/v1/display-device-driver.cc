// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/display-device-driver.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstring>
#include <memory>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/virtio-guest/v1/display-controller-banjo.h"

namespace virtio_display {

// static
zx::result<> DisplayDeviceDriver::Create(zx_device_t* parent,
                                         DisplayControllerBanjo* display_controller_banjo) {
  fbl::AllocChecker alloc_checker;
  auto display_device_driver = fbl::make_unique_checked<DisplayDeviceDriver>(
      &alloc_checker, parent, display_controller_banjo);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayDeviceDriver");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> bind_result = display_device_driver->Bind();
  if (bind_result.is_error()) {
    zxlogf(ERROR, "Failed to bind DisplayDeviceDriver: %s", bind_result.status_string());
    return bind_result.take_error();
  }

  // devmgr now owns the memory for `display_device_driver`.
  [[maybe_unused]] DisplayDeviceDriver* display_device_released = display_device_driver.release();
  return zx::ok();
}

DisplayDeviceDriver::DisplayDeviceDriver(zx_device_t* parent,
                                         DisplayControllerBanjo* display_controller_banjo)
    : DdkDisplayDeviceType(parent), display_controller_banjo_(*display_controller_banjo) {
  ZX_DEBUG_ASSERT(display_controller_banjo != nullptr);
}

DisplayDeviceDriver::~DisplayDeviceDriver() = default;

zx_status_t DisplayDeviceDriver::DdkGetProtocol(uint32_t proto_id, void* out) {
  return display_controller_banjo_.DdkGetProtocol(proto_id, out);
}

zx::result<> DisplayDeviceDriver::Bind() {
  return zx::make_result(DdkAdd(
      ddk::DeviceAddArgs("virtio-gpu-display").set_proto_id(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL)));
}

void DisplayDeviceDriver::DdkRelease() { delete this; }

}  // namespace virtio_display
