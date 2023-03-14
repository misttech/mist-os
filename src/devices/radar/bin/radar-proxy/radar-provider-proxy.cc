// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "radar-provider-proxy.h"

#include <lib/syslog/cpp/macros.h>

namespace radar {

RadarProviderProxy::RadarProviderProxy(RadarDeviceConnector* connector) : connector_(connector) {
  radar_client_.set_error_handler([&](zx_status_t status) { ErrorHandler(status); });
}

void RadarProviderProxy::Connect(
    fidl::InterfaceRequest<fuchsia::hardware::radar::RadarBurstReader> server,
    ConnectCallback callback) {
  if (!radar_client_.is_bound()) {
    fuchsia::hardware::radar::RadarBurstReaderProvider_Connect_Result result;
    result.set_err(fuchsia::hardware::radar::StatusCode::BIND_ERROR);
    callback(std::move(result));
    return;
  }

  radar_client_->Connect(
      std::move(server),
      [cb = std::move(callback)](
          fuchsia::hardware::radar::RadarBurstReaderProvider_Connect_Result result) {
        cb(std::move(result));
      });
}

void RadarProviderProxy::DeviceAdded(int dir_fd, const std::string& filename) {
  if (!radar_client_.is_bound()) {
    connector_->ConnectToRadarDevice(dir_fd, filename, [&](auto client_end) {
      radar_client_.Bind(std::move(client_end));
      return true;
    });
  }
}

void RadarProviderProxy::ErrorHandler(zx_status_t status) {
  FX_PLOGS(ERROR, status) << "Connection to radar device closed, attempting to reconnect";
  // Check for available devices now, just in case one was added before the connection closed. If
  // not, the DeviceWatcher will signal to connect when a new device becomes available.
  connector_->ConnectToFirstRadarDevice([&](auto client_end) {
    radar_client_.Bind(std::move(client_end));
    return true;
  });
}

}  // namespace radar
