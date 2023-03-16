// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/registers.h"

namespace fusb302 {

const char* SwitchBlockConfigToString(SwitchBlockConfig config) {
  switch (config) {
    case SwitchBlockConfig::kOpen:
      return "Open";
    case SwitchBlockConfig::kPullUp:
      return "Pull-up (Pu current source)";
    case SwitchBlockConfig::kPullDown:
      return "Pull-down (Pd resistor)";
    case SwitchBlockConfig::kConnectorVoltage:
      return "VCONN";
  }
  ZX_DEBUG_ASSERT_MSG(false, "Invalid SwitchBlockConfig: %" PRId8, config);
  return nullptr;
}

}  // namespace fusb302
