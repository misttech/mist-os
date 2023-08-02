// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "power_source_state.h"

#include <lib/driver/logging/cpp/structured_logger.h>

namespace fake_battery {

void PowerSourceState::NotifyObservers() {
  // Make a copy of the observers so each observer can remove themselves after notify.
  // Clients need to add themselves each time in order to be notified.
  auto observers = observers_;
  for (Observer* observer : observers) {
    observer->Notify();
  }
}

void PowerSourceState::AddObserver(Observer* o) { observers_.insert(o); }

void PowerSourceState::RemoveObserver(Observer* observer) {
  // There is no need to check the result equal to 1 or not, because if the observer is already
  // notified, then it is expected that the list doesn't contain the element.
  observers_.erase(observer);
}

void PowerSourceState::set_battery_info(const fuchsia_hardware_powersource::BatteryInfo& info) {
  battery_info_ = info;
}

}  // namespace fake_battery
