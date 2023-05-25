// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fan-controller.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

namespace fan_controller {

zx::result<fidl::ClientEnd<fuchsia_thermal::ClientStateWatcher>> FanController::ConnectToWatcher(
    const std::string& client_type) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_thermal::ClientStateWatcher>();
  if (endpoints.is_error()) {
    FX_LOGS(ERROR) << "Could not create endpoints " << endpoints.status_string();
    return zx::error(endpoints.error_value());
  }
  auto result = connector_->Connect({client_type, std::move(endpoints->server)});
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Could not connect to fuchsia_thermal::ClientStateWatcher "
                   << result.error_value();
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(std::move(endpoints->client));
}

void FanController::ExistsCallback(const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                                   const std::string& filename) {
  zx::result client_end = component::ConnectAt<fuchsia_hardware_fan::Device>(dir, filename);
  if (client_end.is_error()) {
    FX_LOGS(ERROR) << "Could not connect to " << filename << ": " << client_end.status_string();
    return;
  }

  auto fan = fidl::SyncClient(std::move(*client_end));
  auto client_type = fan->GetClientType();
  if (client_type.is_error()) {
    FX_LOGS(ERROR) << "Could not get client type " << client_type.error_value();
    return;
  }

  if (controllers_.find(client_type->client_type()) == controllers_.end()) {
    auto watcher = ConnectToWatcher(client_type->client_type());
    if (watcher.is_error()) {
      FX_LOGS(ERROR) << "Could not connect to ClientStateWatcher " << watcher.status_string();
      return;
    }
    controllers_[client_type->client_type()].watcher_.Bind(std::move(*watcher), dispatcher_);
    controllers_[client_type->client_type()].watcher_->Watch().Then(
        fit::bind_member<&FanController::ControllerInstance::WatchCallback>(
            &controllers_[client_type->client_type()]));
  }
  controllers_[client_type->client_type()].fans_.emplace_back(std::move(fan));
}

void FanController::ControllerInstance::WatchCallback(
    fidl::Result<fuchsia_thermal::ClientStateWatcher::Watch>& result) {
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Watch failed with " << result.error_value();
    return;
  }

  watcher_->Watch().Then(fit::bind_member<&FanController::ControllerInstance::WatchCallback>(this));

  if (result->state() > UINT32_MAX) {
    FX_LOGS(ERROR) << "Unable to set state to " << result->state();
    return;
  }
  auto state = static_cast<uint32_t>(result->state());

  fans_.remove_if([state](auto& fan) {
    auto result = fan->SetFanLevel(state);
    if (result.is_error()) {
      FX_LOGS(ERROR) << "SetFanLevel failed with " << result.error_value();
      // FIDL connection failed. Fan has gone away. Remove.
      return true;
    }

    return false;
  });
}

}  // namespace fan_controller
