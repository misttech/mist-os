// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_DRIVER_H_

#include <lib/ddk/driver.h>
#include <lib/mmio/mmio-buffer.h>

#include <memory>

#include <ddktl/device.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/simple/simple-display.h"

namespace simple_display {

class SimpleDisplayDriver;
using DeviceType = ddk::Device<SimpleDisplayDriver, ddk::GetProtocolable>;

// Integration between display drivers and the Driver Framework (v1).
class SimpleDisplayDriver : public DeviceType {
 public:
  explicit SimpleDisplayDriver(zx_device_t* parent, std::string device_name);

  SimpleDisplayDriver(const SimpleDisplayDriver&) = delete;
  SimpleDisplayDriver(SimpleDisplayDriver&&) = delete;
  SimpleDisplayDriver& operator=(const SimpleDisplayDriver&) = delete;
  SimpleDisplayDriver& operator=(SimpleDisplayDriver&&) = delete;

  virtual ~SimpleDisplayDriver();

  // ddk::Device:
  void DdkRelease();

  // ddk::GetProtocolable:
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_proto);

  // Called exactly once before the driver acquires any resource.
  virtual zx::result<> ConfigureHardware() = 0;

  virtual zx::result<fdf::MmioBuffer> GetFrameBufferMmioBuffer() = 0;

  virtual zx::result<DisplayProperties> GetDisplayProperties() = 0;

  // Initialize hardware resources and binds the driver to the device node.
  zx::result<> Initialize();

 private:
  zx::result<std::unique_ptr<SimpleDisplay>> CreateAndInitializeSimpleDisplay();

  std::string device_name_;

  std::unique_ptr<SimpleDisplay> simple_display_;
};

}  // namespace simple_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_DRIVER_H_
