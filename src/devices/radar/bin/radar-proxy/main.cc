// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <sys/stat.h>
#include <sys/types.h>

#include "radar-provider-proxy.h"
#include "radar-proxy.h"
#include "src/lib/fsl/io/device_watcher.h"

namespace radar {

class DefaultRadarDeviceConnector : public RadarDeviceConnector {
 public:
  void ConnectToRadarDevice(int dir_fd, const std::filesystem::path& path,
                            ConnectDeviceCallback connect_device) override {
    fdio_cpp::UnownedFdioCaller caller(dir_fd);
    zx::result endpoints =
        fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
    if (endpoints.is_error()) {
      return;
    }

    zx_status_t status = fdio_service_connect_at(caller.directory().channel()->get(), path.c_str(),
                                                 endpoints->server.TakeChannel().release());
    if (status == ZX_OK) {
      connect_device(std::move(endpoints->client));
    }
  }

  void ConnectToFirstRadarDevice(ConnectDeviceCallback connect_device) override {
    DIR* const devices_dir = opendir(RadarProxy::kRadarDeviceDirectory);
    if (!devices_dir) {
      return;
    }

    for (const dirent* device = readdir(devices_dir); device; device = readdir(devices_dir)) {
      bool found = false;
      ConnectToRadarDevice(dirfd(devices_dir), device->d_name, [&](auto client_end) {
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
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  radar::DefaultRadarDeviceConnector connector;
  radar::RadarProviderProxy proxy(loop.dispatcher(), &connector);

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());

  zx::result result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << result.status_string();
    return -1;
  }

  result = outgoing.AddUnmanagedProtocol<fuchsia_hardware_radar::RadarBurstReaderProvider>(
      [&](fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstReaderProvider> server_end) {
        fidl::BindServer(loop.dispatcher(), std::move(server_end), &proxy);
      });

  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add RadarBurstReaderProvider protocol: " << result.status_string();
    return -1;
  }

  return loop.Run();
}
