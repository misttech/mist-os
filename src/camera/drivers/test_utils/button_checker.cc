// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "button_checker.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/debug.h>

#include <cstring>
#include <iostream>
#include <string>

std::unique_ptr<ButtonChecker> ButtonChecker::Create() {
  auto checker = std::make_unique<ButtonChecker>();
  auto status = checker->loop_.Run();
  if (status != ZX_ERR_CANCELED) {
    zxlogf(ERROR, "Could not run button checker %i", status);
    return nullptr;
  }

  if (checker->devices_.empty()) {
    zxlogf(WARNING, "Zero devices were bound from %s", kDevicePath);
    return nullptr;
  }

  return checker;
}

ButtonChecker::ButtonState ButtonChecker::GetMuteState() {
  auto state = ButtonState::UNKNOWN;
  for (auto& device : devices_) {
    // Get the report for the mute field.
    auto result = device->GetInputReport(fuchsia_input_report::DeviceType::kConsumerControl);
    if (result.is_error()) {
      auto error_str = result.error_value().FormatDescription();
      zxlogf(ERROR, "GetInputReport failed %s", error_str.c_str());
      return ButtonState::UNKNOWN;
    }

    const auto& consumer_control = result->report().consumer_control();
    if (!consumer_control) {
      zxlogf(ERROR, "Invalid input report. Must have consumer_control.");
      return ButtonState::UNKNOWN;
    }
    const auto& pressed_buttons = consumer_control->pressed_buttons();
    if (!pressed_buttons) {
      zxlogf(ERROR, "Invalid input report. Must have pressed_buttons.");
      return ButtonState::UNKNOWN;
    }
    auto state_for_device =
        (std::find(pressed_buttons->begin(), pressed_buttons->end(),
                   fuchsia_input_report::ConsumerControlButton::kMicMute) != pressed_buttons->end())
            ? ButtonState::DOWN
            : ButtonState::UP;

    // Make sure that devices don't have conflicting states.
    if (state != ButtonState::UNKNOWN && state != state_for_device) {
      zxlogf(ERROR, "Conflicting states reported by different devices");
      return ButtonState::UNKNOWN;
    }
    state = state_for_device;
  }

  return state;
}

void ButtonChecker::ExistsCallback(const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                                   const std::string& filename) {
  zxlogf(DEBUG, "Reading reports from %s", filename.c_str());

  zx::result connection = component::ConnectAt<fuchsia_input_report::InputDevice>(dir, filename);
  if (connection.is_error()) {
    zxlogf(ERROR, "Could not open %s: %s", filename.c_str(), connection.status_string());
    return;
  }

  auto device = fidl::SyncClient(std::move(connection.value()));
  const auto descriptor = device->GetDescriptor();
  if (descriptor.is_error()) {
    auto error_str = descriptor.error_value().FormatDescription();
    zxlogf(ERROR, "GetDescriptor failed for %s: %s", filename.c_str(), error_str.c_str());
    return;
  }

  // Find mute button and if it exists, add to list of devices.
  const auto& consumer_control = descriptor->descriptor().consumer_control();
  if (!consumer_control) {
    return;
  }
  const auto& input = consumer_control->input();
  if (!input) {
    return;
  }
  const auto& buttons = input->buttons();
  if (!buttons) {
    return;
  }
  if (std::find(buttons->begin(), buttons->end(),
                fuchsia_input_report::ConsumerControlButton::kMicMute) != buttons->end()) {
    devices_.emplace_back(std::move(device));
  }
}

void ButtonChecker::IdleCallback() {
  // Once we've found all the existing devices, stop watching.
  loop_.Quit();
}

bool VerifyDeviceUnmuted(bool consider_unknown_as_unmuted) {
  auto state = ButtonChecker::ButtonState::UNKNOWN;
  auto checker = ButtonChecker::Create();
  if (checker) {
    state = checker->GetMuteState();
  }
  if (state == ButtonChecker::ButtonState::UP) {
    return true;
  }
  if (state == ButtonChecker::ButtonState::UNKNOWN) {
    std::cerr << "**************************************************\n"
                 "* WARNING: DEVICE MUTE STATE UNKNOWN. CAMERA MAY *\n"
                 "*          NOT OPERATE AND TESTS MAY BE SKIPPED! *\n"
                 "**************************************************\n";
    std::cerr.flush();
    return consider_unknown_as_unmuted;
  }
  ZX_DEBUG_ASSERT(state == ButtonChecker::ButtonState::DOWN);
  std::cerr << "**********************************************\n"
               "* WARNING: DEVICE IS MUTED. CAMERA WILL NOT  *\n"
               "*          OPERATE AND TESTS MAY BE SKIPPED! *\n"
               "**********************************************\n";
  std::cerr.flush();
  return false;
}
