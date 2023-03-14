// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROVIDER_PROXY_H_
#define SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROVIDER_PROXY_H_

#include <lib/async/dispatcher.h>

#include "radar-proxy.h"

namespace radar {

class RadarProviderProxy
    : public RadarProxy,
      public fidl::AsyncEventHandler<fuchsia_hardware_radar::RadarBurstReaderProvider> {
 public:
  RadarProviderProxy(async_dispatcher_t* dispatcher, RadarDeviceConnector* connector)
      : dispatcher_(dispatcher), connector_(connector) {}

  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override;

  void DeviceAdded(int dir_fd, const std::string& filename) override;

  void on_fidl_error(fidl::UnbindInfo info) override;

 private:
  async_dispatcher_t* const dispatcher_;
  RadarDeviceConnector* const connector_;
  fidl::Client<fuchsia_hardware_radar::RadarBurstReaderProvider> radar_client_;
};

}  // namespace radar

#endif  // SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROVIDER_PROXY_H_
