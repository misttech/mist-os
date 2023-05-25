// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROXY_H_
#define SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROXY_H_

#include <fidl/fuchsia.hardware.radar/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>

#include <memory>
#include <string>

#include "sdk/lib/component/outgoing/cpp/outgoing_directory.h"
#include "src/lib/fsl/io/device_watcher.h"

namespace radar {

class RadarDeviceConnector {
 public:
  // Called on zero or more eligible radar devices. If true is returned, the device is usable and
  // the callback should not be invoked again. Otherwise, the device is not usable, and the search
  // for a suitable device should continue.
  using ConnectDeviceCallback =
      fit::function<bool(fidl::ClientEnd<fuchsia_hardware_radar::RadarBurstReaderProvider>)>;

  // Synchronously connects to the given radar device and passes the client end to the callback. The
  // callback is not called if the connection could not be made. The callback return value is
  // ignored.
  virtual void ConnectToRadarDevice(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                                    const std::string& path,
                                    ConnectDeviceCallback connect_device) = 0;

  // Calls ConnectToRadarDevice() on available devices until the callback returns true.
  virtual void ConnectToFirstRadarDevice(ConnectDeviceCallback connect_device) = 0;
};

class RadarProxy : public fidl::Server<fuchsia_hardware_radar::RadarBurstReaderProvider> {
 public:
  static constexpr char kRadarDeviceDirectory[] = "/dev/class/radar";

  static std::unique_ptr<RadarProxy> Create(async_dispatcher_t* dispatcher,
                                            RadarDeviceConnector* connector);

  RadarProxy()
      : device_watcher_(fsl::DeviceWatcher::Create(
            kRadarDeviceDirectory,
            [&](const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) {
              DeviceAdded(dir, filename);
            })) {}

  // Called by a DeviceWatcher when /dev/class/radar has a new device, and for each existing device
  // during construction.
  virtual void DeviceAdded(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                           const std::string& filename) = 0;

  virtual zx::result<> AddProtocols(component::OutgoingDirectory* outgoing) = 0;

 private:
  std::unique_ptr<fsl::DeviceWatcher> device_watcher_;
};

}  // namespace radar

#endif  // SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_PROXY_H_
