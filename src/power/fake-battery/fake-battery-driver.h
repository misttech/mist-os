// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_FAKE_BATTERY_FAKE_BATTERY_DRIVER_H_
#define SRC_POWER_FAKE_BATTERY_FAKE_BATTERY_DRIVER_H_

#include <fidl/fuchsia.hardware.powersource/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.powersource/cpp/wire.h>
#include <fidl/fuchsia.power.battery/cpp/wire.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>

namespace fake_battery {

// Protocol served to client components over devfs.
class PowerSourceProtocolServer : public fidl::WireServer<fuchsia_hardware_powersource::Source> {
 public:
  explicit PowerSourceProtocolServer() = default;

  void GetPowerInfo(GetPowerInfoCompleter::Sync& completer) override;

  void GetStateChangeEvent(GetStateChangeEventCompleter::Sync& completer) override;

  void GetBatteryInfo(GetBatteryInfoCompleter::Sync& completer) override;
};

class Driver;

}  // namespace fake_battery

#endif  // SRC_POWER_FAKE_BATTERY_FAKE_BATTERY_DRIVER_H_
