// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "power_source_protocol_server.h"

#include <fidl/fuchsia.hardware.powersource/cpp/natural_types.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

using fuchsia_hardware_powersource::PowerType;
using fuchsia_hardware_powersource::SourceInfo;

namespace fake_powersource {

PowerSourceProtocolServer::PowerSourceProtocolServer(std::shared_ptr<PowerSourceState> state)
    : state_(std::move(state)) {
  zx::event::create(0, &state_event_);
}

PowerSourceProtocolServer::~PowerSourceProtocolServer() { state_->RemoveObserver(this); }

zx_status_t PowerSourceProtocolServer::ClearSignal() {
  zx_status_t status = state_event_.signal(ZX_USER_SIGNAL_0, 0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to clear signal on event: %s", zx_status_get_string(status));
  }
  // For now, a client calls GetStateChangeEvent and then waits
  // Add to observers_ so it can be notified later when the fake data changes.
  // At that time, SignalClient will be called to unblock client so the client can call
  // GetStateChangeEvent again...
  // It will make better sense to use Hanging Get.
  state_->AddObserver(this);
  return status;
}

zx_status_t PowerSourceProtocolServer::SignalClient() {
  zx_status_t status = state_event_.signal(0, ZX_USER_SIGNAL_0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to set signal on event: %s", zx_status_get_string(status));
  }
  state_->RemoveObserver(this);
  return status;
}

// TODO(https://fxbug.dev/42081644): when a cli is being developed, this will be made adjustable.
void PowerSourceProtocolServer::GetPowerInfo(GetPowerInfoCompleter::Sync& completer) {
  ClearSignal();
  completer.Reply({ZX_OK, state_->source_info()});
}

void PowerSourceProtocolServer::GetStateChangeEvent(GetStateChangeEventCompleter::Sync& completer) {
  zx::event clone;
  zx_status_t status = state_event_.duplicate(ZX_RIGHT_WAIT | ZX_RIGHT_TRANSFER, &clone);
  if (status == ZX_OK) {
    // Clear signal before returning.
    ClearSignal();
  }
  completer.Reply({status, std::move(clone)});
}

void PowerSourceProtocolServer::GetBatteryInfo(GetBatteryInfoCompleter::Sync& completer) {
  ClearSignal();
  if (state_->source_info().type() == fuchsia_hardware_powersource::PowerType::kBattery) {
    completer.Reply({ZX_OK, state_->battery_info()});
  } else {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
}

void PowerSourceProtocolServer::Serve(
    async_dispatcher_t* dispatcher, fidl::ServerEnd<fuchsia_hardware_powersource::Source> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_powersource
