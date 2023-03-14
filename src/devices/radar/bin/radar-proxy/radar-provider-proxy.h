// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROVIDER_PROXY_H_
#define SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROVIDER_PROXY_H_

#include "radar-proxy.h"

namespace radar {

class RadarProviderProxy : public RadarProxy {
 public:
  explicit RadarProviderProxy(RadarDeviceConnector* connector);

  void Connect(fidl::InterfaceRequest<fuchsia::hardware::radar::RadarBurstReader> server,
               ConnectCallback callback) override;

  void DeviceAdded(int dir_fd, const std::string& filename) override;

 private:
  void ErrorHandler(zx_status_t status);

  RadarDeviceConnector* const connector_;
  fuchsia::hardware::radar::RadarBurstReaderProviderPtr radar_client_;
};

}  // namespace radar

#endif  // SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROVIDER_PROXY_H_
