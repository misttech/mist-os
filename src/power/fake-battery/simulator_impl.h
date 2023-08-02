// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_FAKE_BATTERY_SIMULATOR_IMPL_H_
#define SRC_POWER_FAKE_BATTERY_SIMULATOR_IMPL_H_

#include <fidl/fuchsia.hardware.powersource.test/cpp/fidl.h>

#include "fidl/fuchsia.hardware.powersource.test/cpp/natural_types.h"

namespace fake_battery {

class PowerSourceProtocolServer;
class PowerSourceState;
class SimulatorImpl : public fidl::Server<fuchsia_hardware_powersource_test::SourceSimulator> {
 public:
  explicit SimulatorImpl() = default;
  ~SimulatorImpl() override;
  explicit SimulatorImpl(std::shared_ptr<PowerSourceState> source);
  void SetPowerInfo(SetPowerInfoRequest& request, SetPowerInfoCompleter::Sync& completer) override;
  void SetBatteryInfo(SetBatteryInfoRequest& request,
                      SetBatteryInfoCompleter::Sync& completer) override;

 private:
  std::shared_ptr<PowerSourceState> source_state_;
};

}  // namespace fake_battery

#endif  // SRC_POWER_FAKE_BATTERY_SIMULATOR_IMPL_H_
