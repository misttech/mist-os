// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CAMERA_DRIVERS_TEST_UTILS_BUTTON_CHECKER_H_
#define SRC_CAMERA_DRIVERS_TEST_UTILS_BUTTON_CHECKER_H_

#include <fidl/fuchsia.input.report/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>

#include <vector>

#include "src/lib/fsl/io/device_watcher.h"

// This class connects to input devices and allows for queries to input buttons or switches.
// Currently, this class only supports checking the state of the mute switch.
class ButtonChecker {
 public:
  static constexpr auto kDevicePath = "/dev/class/input-report";
  ButtonChecker()
      : device_watcher_(fsl::DeviceWatcher::CreateWithIdleCallback(
            kDevicePath, fit::bind_member<&ButtonChecker::ExistsCallback>(this),
            fit::bind_member<&ButtonChecker::IdleCallback>(this), loop_.dispatcher())) {}

  enum class ButtonState {
    UNKNOWN,  // Button state could not be determined or is undefined.
    DOWN,     // Button is pressed, on, or active.
    UP,       // Button is not pressed, off, or inactive.
  };

  // Creates a new ButtonChecker instance. Returns nullptr on failure.
  static std::unique_ptr<ButtonChecker> Create();

  // Gets the state of the Mute button/switch, if available.
  ButtonState GetMuteState();

 private:
  // Function called when a device is found. Checks device's descriptor for the mute button. If mute
  // button is found adds it to devices_.
  void ExistsCallback(const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                      const std::string& filename);
  // Function called when all devices are found. Stops device_watcher_.
  void IdleCallback();

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::unique_ptr<fsl::DeviceWatcher> device_watcher_;
  std::vector<fidl::SyncClient<fuchsia_input_report::InputDevice>> devices_;
};

// Convenience wrapper to check the mute state of a device. Returns true if the device is confirmed
// to be unmuted. If the device is muted or its mute state could not be determined, a warning is
// printed to stderr.
bool VerifyDeviceUnmuted(bool consider_unknown_as_unmuted = false);

#endif  // SRC_CAMERA_DRIVERS_TEST_UTILS_BUTTON_CHECKER_H_
