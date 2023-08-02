// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_FAKE_BATTERY_POWER_SOURCE_PROTOCOL_SERVER_H_
#define SRC_POWER_FAKE_BATTERY_POWER_SOURCE_PROTOCOL_SERVER_H_

#include <fidl/fuchsia.hardware.powersource/cpp/fidl.h>
#include <fidl/fuchsia.hardware.powersource/cpp/natural_types.h>
#include <lib/zx/event.h>
#include <zircon/types.h>

#include "src/power/fake-battery/power_source_state.h"

namespace fake_battery {

class PowerSourceState;

// Protocol served to client components over devfs.
class PowerSourceProtocolServer : public fidl::Server<fuchsia_hardware_powersource::Source>,
                                  public Observer {
 public:
  explicit PowerSourceProtocolServer(std::shared_ptr<PowerSourceState> state);
  ~PowerSourceProtocolServer() override;

  // Sets a signal on state_event_, notifying clients that power source state has changed.
  zx_status_t SignalClient();
  // Clears the above state.
  zx_status_t ClearSignal();

  void GetPowerInfo(GetPowerInfoCompleter::Sync& completer) override;

  // TODO(bihai): There is no unit test on this function yet. We will deprecated this and soft
  // transition to a new set of API.
  void GetStateChangeEvent(GetStateChangeEventCompleter::Sync& completer) override;

  void GetBatteryInfo(GetBatteryInfoCompleter::Sync& completer) override;

  void Notify() override { SignalClient(); }

 private:
  std::shared_ptr<PowerSourceState> state_;
  zx::event state_event_;
};

}  // namespace fake_battery

#endif  // SRC_POWER_FAKE_BATTERY_POWER_SOURCE_PROTOCOL_SERVER_H_
