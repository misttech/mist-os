// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <memory>

#include "radar-provider-proxy.h"
#include "radar-proxy.h"
#include "radar-reader-proxy.h"
#include "src/devices/radar/bin/radar-proxy/config.h"
#include "src/lib/fsl/io/device_watcher.h"

namespace radar {

class DefaultRadarDeviceConnector : public RadarDeviceConnector {
 public:
  void ConnectToRadarDevice(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                            const std::string& path,
                            ConnectDeviceCallback connect_device) override {
    zx::result client_end =
        component::ConnectAt<fuchsia_hardware_radar::RadarBurstReaderProvider>(dir, path);
    if (client_end.is_error()) {
      return;
    }
    connect_device(std::move(client_end.value()));
  }

  void ConnectToFirstRadarDevice(ConnectDeviceCallback connect_device) override {
    // TODO(https://fxbug.dev/113882): Use device_watcher's waiting helper when it exists.
    DIR* const devices_dir = opendir(RadarProxy::kRadarDeviceDirectory);
    if (!devices_dir) {
      return;
    }
    fdio_cpp::UnownedFdioCaller caller(dirfd(devices_dir));

    for (const dirent* device = readdir(devices_dir); device; device = readdir(devices_dir)) {
      bool found = false;
      ConnectToRadarDevice(caller.directory(), device->d_name, [&](auto client_end) {
        return found = connect_device(std::move(client_end));
      });
      if (found) {
        break;
      }
    }

    closedir(devices_dir);
  }
};

}  // namespace radar

int main(int argc, const char** argv) {
  radar::DefaultRadarDeviceConnector connector;
  std::unique_ptr<radar::RadarProxy> proxy;

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  proxy = radar::RadarProxy::Create(loop.dispatcher(), &connector);

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());

  zx::result result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << result.status_string();
    return -1;
  }

  result = outgoing.AddUnmanagedProtocol<fuchsia_hardware_radar::RadarBurstReaderProvider>(
      [&](fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstReaderProvider> server_end) {
        fidl::BindServer(loop.dispatcher(), std::move(server_end), proxy.get());
      });

  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add RadarBurstReaderProvider protocol: " << result.status_string();
    return -1;
  }

  // TODO(fxbug.dev/100020): Structured config isn't really needed now that there are two separate
  // build targets for the reader/provider proxying cases. Make the proxy implementation handle this
  // instead.
  const auto config = config::Config::TakeFromStartupHandle();
  if (config.proxy_radar_burst_reader()) {
    result = outgoing.AddUnmanagedProtocol<fuchsia_hardware_radar::RadarBurstInjector>(
        [&](fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstInjector> server_end) {
          proxy->BindInjector(std::move(server_end));
        });

    if (result.is_error()) {
      FX_LOGS(ERROR) << "Failed to add RadarBurstInjector protocol: " << result.status_string();
      return -1;
    }
  }

  return loop.Run();
}
