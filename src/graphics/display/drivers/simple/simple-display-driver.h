// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_DRIVER_H_

#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/mmio/mmio-buffer.h>

#include <memory>
#include <optional>

#include "src/graphics/display/drivers/simple/simple-display.h"

namespace simple_display {

// Integration between display drivers and the Driver Framework (v1).
class SimpleDisplayDriver : public fdf::DriverBase {
 public:
  explicit SimpleDisplayDriver(std::string_view device_name, fdf::DriverStartArgs start_args,
                               fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  SimpleDisplayDriver(const SimpleDisplayDriver&) = delete;
  SimpleDisplayDriver(SimpleDisplayDriver&&) = delete;
  SimpleDisplayDriver& operator=(const SimpleDisplayDriver&) = delete;
  SimpleDisplayDriver& operator=(SimpleDisplayDriver&&) = delete;

  virtual ~SimpleDisplayDriver();

  // fdf::DriverBase:
  zx::result<> Start() override;
  void Stop() override;

  // Called exactly once before the driver acquires any resource.
  virtual zx::result<> ConfigureHardware() = 0;

  virtual zx::result<fdf::MmioBuffer> GetFrameBufferMmioBuffer() = 0;

  virtual zx::result<DisplayProperties> GetDisplayProperties() = 0;

 private:
  zx::result<std::unique_ptr<SimpleDisplay>> CreateAndInitializeSimpleDisplay();

  // Must be called after `simple_display_` is initialized.
  zx::result<> InitializeBanjoServerNode();

  std::unique_ptr<SimpleDisplay> simple_display_;

  std::optional<compat::BanjoServer> banjo_server_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace simple_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_DRIVER_H_
