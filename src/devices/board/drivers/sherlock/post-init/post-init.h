// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_SHERLOCK_POST_INIT_POST_INIT_H_
#define SRC_DEVICES_BOARD_DRIVERS_SHERLOCK_POST_INIT_POST_INIT_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/wire.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/stdcompat/span.h>

namespace sherlock {

class PostInit : public fdf::DriverBase {
 public:
  PostInit(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("post-init", std::move(start_args), std::move(dispatcher)) {}

  void Start(fdf::StartCompleter completer) override;

 private:
  enum SherlockBoardBuild : uint8_t {
    // From the schematic.
    BOARD_REV_B72 = 0x01,
    BOARD_REV_P2 = 0x0B,
    BOARD_REV_REWORK = 0x0C,
    BOARD_REV_P21 = 0x0D,
    BOARD_REV_EVT1 = 0x0E,
    BOARD_REV_EVT2 = 0x0F,
  };

  zx::result<> InitBoardInfo();
  zx::result<> SetBoardInfo();

  // Identifies the panel type and stores it to `panel_type_`.
  // Must be called exactly once during driver `Start()`.
  zx::result<> IdentifyPanel();

  // Must be called after `IdentifyPanel()`.
  zx::result<> InitDisplay();

  // Must be called after `IdentifyPanel()`.
  zx::result<> InitTouch();

  // Must be called after `IdentifyPanel()`.
  zx::result<> InitBacklight();

  zx::result<> SetInspectProperties();

  // Constructs a number using the value of each GPIO as one bit. The order of elements in
  // node_names determines the bits set in the result from LSB to MSB.
  zx::result<uint8_t> ReadGpios(cpp20::span<const char* const> node_names);

  fidl::SyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> controller_;

  fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> pbus_;

  SherlockBoardBuild board_build_{};
  uint8_t board_option_{};
  display::PanelType panel_type_ = display::PanelType::kUnknown;

  std::unique_ptr<inspect::ComponentInspector> component_inspector_;

  inspect::Inspector inspector_;
  inspect::Node root_;
  inspect::UintProperty board_rev_property_;
  inspect::UintProperty board_option_property_;
  inspect::UintProperty panel_type_property_;
};

}  // namespace sherlock

#endif  // SRC_DEVICES_BOARD_DRIVERS_SHERLOCK_POST_INIT_POST_INIT_H_
