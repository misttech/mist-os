// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_DEVICE_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_DEVICE_DRIVER_H_

#include <lib/ddk/device.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <cstdint>

#include <ddktl/device.h>

#include "src/graphics/display/drivers/virtio-guest/v1/display-controller-banjo.h"

namespace virtio_display {

class DisplayDeviceDriver;
using DdkDisplayDeviceType = ddk::Device<DisplayDeviceDriver, ddk::GetProtocolable>;

// Integration between the display sub-device and the Driver Framework.
class DisplayDeviceDriver : public DdkDisplayDeviceType {
 public:
  // Factory method used by the device manager glue code.
  //
  // `display_controller_banjo` must be non-null and outlive the created
  // `DisplayDeviceDriver`. This can be achieved if the `parent` device owns
  // the `display_controller_banjo`.
  static zx::result<> Create(zx_device_t* parent, DisplayControllerBanjo* display_controller_banjo);

  // Exposed for testing. Production code must use the Create() factory method.
  //
  // `display_controller_banjo` must be non-null and outlive the created
  // `DisplayDeviceDriver`. This can be achieved if the `parent` device owns
  // the `display_controller_banjo`.
  explicit DisplayDeviceDriver(zx_device_t* parent,
                               DisplayControllerBanjo* display_controller_banjo);

  DisplayDeviceDriver(const DisplayDeviceDriver&) = delete;
  DisplayDeviceDriver& operator=(const DisplayDeviceDriver&) = delete;
  DisplayDeviceDriver(DisplayDeviceDriver&&) = delete;
  DisplayDeviceDriver& operator=(DisplayDeviceDriver&&) = delete;

  ~DisplayDeviceDriver();

  // ddk::GetProtocolable interface.
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  // ddk::Device interface.
  void DdkRelease();

 private:
  zx::result<> Bind();

  DisplayControllerBanjo& display_controller_banjo_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_DEVICE_DRIVER_H_
