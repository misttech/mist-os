// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_DRIVER_H_

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/zx/result.h>

#include <memory>
#include <optional>

#include "src/graphics/display/drivers/goldfish-display/display-engine.h"

namespace goldfish {

class DisplayDriver : public fdf::DriverBase {
 public:
  explicit DisplayDriver(fdf::DriverStartArgs start_args,
                         fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  DisplayDriver(const DisplayDriver&) = delete;
  DisplayDriver(DisplayDriver&&) = delete;
  DisplayDriver& operator=(const DisplayDriver&) = delete;
  DisplayDriver& operator=(DisplayDriver&&) = delete;

  ~DisplayDriver() override;

  // fdf::DriverBase:
  zx::result<> Start() override;

 private:
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  // Must outlive `display_engine_`.
  fdf::SynchronizedDispatcher display_event_dispatcher_;

  // Must outlive `banjo_server_`.
  std::unique_ptr<DisplayEngine> display_engine_;

  std::optional<compat::BanjoServer> banjo_server_;
};

}  // namespace goldfish

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_DRIVER_H_
