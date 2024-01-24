// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/manufacturer_names.h"

#include <string_view>

namespace bt {

// This file used to contain a list of string identifiers for converting a
// manufacturer ID to a human readable string identifying the manufacturer.
// This is being refactored and the function contained here will likely be
// removed soon.
//
// The manufacturer's identifiers can be found in the Bluetooth SIG Assigned
// Numbers document, see
// https://www.bluetooth.com/specifications/assigned-numbers/company-identifiers.
//
// TODO(https://fxbug.dev/321947674) - Remove or rename.
std::string GetManufacturerName(uint16_t manufacturer_id) {
  char buffer[std::string_view("0x0000").size() + 1];
  snprintf(buffer, sizeof(buffer), "0x%04x", manufacturer_id);
  return std::string(buffer);
}

}  // namespace bt
