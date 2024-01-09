// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_ASTRO_POST_INIT_POST_INIT_H_
#define SRC_DEVICES_BOARD_DRIVERS_ASTRO_POST_INIT_POST_INIT_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/stdcompat/span.h>

namespace astro {

class PostInit : public fdf::DriverBase {
 public:
  PostInit(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("post-init", std::move(start_args), std::move(dispatcher)) {}

  void Start(fdf::StartCompleter completer) override;

 private:
  // Astro Board Revs
  enum AstroBoardBuild : uint8_t {
    BOARD_REV_P1 = 0,
    BOARD_REV_P2 = 1,
    BOARD_REV_EVT_1 = 2,
    BOARD_REV_EVT_2 = 3,
    BOARD_REV_DVT = 4,
    BOARD_REV_PVT = 5,

    MAX_SUPPORTED_REV,  // This must be last entry
  };

  zx::result<> InitBoardInfo();
  zx::result<> SetBoardInfo();
  zx::result<> InitDisplay();

  // Constructs a number using the value of each GPIO as one bit. The order of elements in
  // node_names determines the bits set in the result from LSB to MSB.
  zx::result<uint8_t> ReadGpios(cpp20::span<const char* const> node_names);

  fidl::SyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> controller_;

  fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> pbus_;
  fidl::SyncClient<fuchsia_driver_framework::CompositeNodeManager> composite_manager_;

  AstroBoardBuild board_build_{};
  uint8_t display_id_{};
};

}  // namespace astro

#endif  // SRC_DEVICES_BOARD_DRIVERS_ASTRO_POST_INIT_POST_INIT_H_
