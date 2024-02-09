// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "simulator_impl.h"

#include <fidl/fuchsia.hardware.powersource/cpp/wire.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include "power_source_state.h"

namespace fake_powersource {

SimulatorImpl::SimulatorImpl(std::shared_ptr<PowerSourceState> source)
    : source_state_(std::move(source)) {}

SimulatorImpl::~SimulatorImpl() = default;

// TODO(b/289224085): this will be implemented later when a cli is being developed.
void SimulatorImpl::SetPowerInfo(SetPowerInfoRequest& request,
                                 SetPowerInfoCompleter::Sync& completer) {
  auto info = request.info();
}

void SimulatorImpl::SetBatteryInfo(SetBatteryInfoRequest& request,
                                   SetBatteryInfoCompleter::Sync& completer) {
  this->source_state_->set_battery_info(request.info());

  if (source_state_)
    source_state_->NotifyObservers();
}

void SimulatorImpl::Serve(
    async_dispatcher_t* dispatcher,
    fidl::ServerEnd<fuchsia_hardware_powersource_test::SourceSimulator> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_powersource
