// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_THERMAL_BIN_FAN_CONTROLLER_FAN_CONTROLLER_H_
#define SRC_DEVICES_THERMAL_BIN_FAN_CONTROLLER_FAN_CONTROLLER_H_

#include <fidl/fuchsia.hardware.fan/cpp/fidl.h>
#include <fidl/fuchsia.thermal/cpp/fidl.h>

#include <list>

#include "src/lib/fsl/io/device_watcher.h"

namespace fan_controller {

constexpr char kFanDirectory[] = "/dev/class/fan";

class FanController {
 public:
  explicit FanController(async_dispatcher_t* dispatcher,
                         fidl::ClientEnd<fuchsia_thermal::ClientStateConnector> connector)
      : dispatcher_(dispatcher),
        connector_(std::move(connector)),
        device_watcher_(fsl::DeviceWatcher::Create(
            kFanDirectory, fit::bind_member<&FanController::ExistsCallback>(this), dispatcher)) {}

 private:
  void ExistsCallback(const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                      const std::string& filename);
  zx::result<fidl::ClientEnd<fuchsia_thermal::ClientStateWatcher>> ConnectToWatcher(
      const std::string& client_type);

  async_dispatcher_t* dispatcher_;
  fidl::SyncClient<fuchsia_thermal::ClientStateConnector> connector_;
  std::unique_ptr<fsl::DeviceWatcher> device_watcher_;

  struct ControllerInstance {
    fidl::Client<fuchsia_thermal::ClientStateWatcher> watcher_;
    std::list<const fidl::SyncClient<fuchsia_hardware_fan::Device>> fans_;

    void WatchCallback(fidl::Result<fuchsia_thermal::ClientStateWatcher::Watch>& result);
  };
  std::map<std::string, ControllerInstance> controllers_;
};

}  // namespace fan_controller

#endif  // SRC_DEVICES_THERMAL_BIN_FAN_CONTROLLER_FAN_CONTROLLER_H_
