// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "radar-provider-proxy.h"

#include <lib/syslog/cpp/macros.h>

namespace radar {

void RadarProviderProxy::Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) {
  if (!radar_client_) {
    completer.Reply(fit::error(fuchsia_hardware_radar::StatusCode::kBindError));
    return;
  }

  radar_client_->Connect(std::move(request.server()))
      .Then([async_completer = completer.ToAsync()](const auto& result) mutable {
        if (result.is_ok()) {
          async_completer.Reply(fit::ok());
        } else if (result.error_value().is_domain_error()) {
          async_completer.Reply(fit::error(result.error_value().domain_error()));
        } else {
          async_completer.Reply(fit::error(fuchsia_hardware_radar::StatusCode::kBindError));
        }
      });
}

void RadarProviderProxy::DeviceAdded(int dir_fd, const std::string& filename) {
  if (!radar_client_) {
    connector_->ConnectToRadarDevice(dir_fd, filename, [&](auto client_end) {
      radar_client_.Bind(std::move(client_end), dispatcher_, this);
      return true;
    });
  }
}

void RadarProviderProxy::on_fidl_error(fidl::UnbindInfo info) {
  FX_PLOGS(ERROR, info.status()) << "Connection to radar device closed, attempting to reconnect";
  // Check for available devices now, just in case one was added before the connection closed. If
  // not, the DeviceWatcher will signal to connect when a new device becomes available.
  connector_->ConnectToFirstRadarDevice([&](auto client_end) {
    radar_client_.Bind(std::move(client_end), dispatcher_, this);
    return true;
  });
}

}  // namespace radar
