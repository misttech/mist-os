// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "power_source_protocol_server.h"

#include <fidl/fuchsia.hardware.powersource/cpp/natural_types.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

using fuchsia_hardware_powersource::wire::BatteryInfo;
using fuchsia_hardware_powersource::wire::BatteryUnit;
using fuchsia_hardware_powersource::wire::PowerType;
using fuchsia_hardware_powersource::wire::SourceInfo;

namespace fake_battery {

zx_status_t PowerSourceProtocolServer::ClearSignal() {
  zx_status_t status = state_event_.signal(ZX_USER_SIGNAL_0, 0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to clear signal on event: %s", zx_status_get_string(status));
  }
  return status;
}

zx_status_t PowerSourceProtocolServer::SignalClient() {
  zx_status_t status = state_event_.signal(0, ZX_USER_SIGNAL_0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to set signal on event: %s", zx_status_get_string(status));
  }
  return status;
}

void PowerSourceProtocolServer::GetPowerInfo(GetPowerInfoCompleter::Sync& completer) {
  SourceInfo source_info{PowerType::kBattery,  // PowerType::kAc,
                         fuchsia_hardware_powersource::kPowerStateCharging |
                             fuchsia_hardware_powersource::kPowerStateOnline};
  completer.Reply(ZX_OK, source_info);
}

void PowerSourceProtocolServer::GetStateChangeEvent(GetStateChangeEventCompleter::Sync& completer) {
  zx::event clone;
  zx_status_t status = state_event_.duplicate(ZX_RIGHT_WAIT | ZX_RIGHT_TRANSFER, &clone);
  if (status == ZX_OK) {
    // Clear signal before returning.
    ClearSignal();
  }
  completer.Reply(status, std::move(clone));
}

void PowerSourceProtocolServer::GetBatteryInfo(GetBatteryInfoCompleter::Sync& completer) {
  BatteryInfo battery_info{
      .unit = BatteryUnit::kMa,
      .design_capacity = 3000,
      .last_full_capacity = 2950,
      .design_voltage = 3000,  // mV
      .capacity_warning = 800,
      .capacity_low = 500,
      .capacity_granularity_low_warning = 20,
      .capacity_granularity_warning_full = 1,
  };

  battery_info.present_rate = 2;
  battery_info.remaining_capacity = 45;
  battery_info.present_voltage = 2900;
  completer.Reply(ZX_OK, battery_info);
}

}  // namespace fake_battery
