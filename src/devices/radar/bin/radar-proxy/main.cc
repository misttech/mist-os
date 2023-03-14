// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sys/cpp/component_context.h>
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
    fidl::InterfaceHandle<fuchsia::hardware::radar::RadarBurstReaderProvider> client_end;
    zx_status_t status = fdio_service_connect_at(caller.directory().channel()->get(), path.c_str(),
                                                 client_end.NewRequest().TakeChannel().release());
    if (status == ZX_OK && client_end.is_valid()) {
      connect_device(std::move(client_end));
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
  radar::RadarProviderProxy proxy(&connector);

  fidl::Binding<fuchsia::hardware::radar::RadarBurstReaderProvider> binding(&proxy);
  fidl::InterfaceRequestHandler<fuchsia::hardware::radar::RadarBurstReaderProvider> handler =
      [&](fidl::InterfaceRequest<fuchsia::hardware::radar::RadarBurstReaderProvider> request) {
        binding.Bind(std::move(request));
      };
  auto context = sys::ComponentContext::CreateAndServeOutgoingDirectory();
  context->outgoing()->AddPublicService(std::move(handler));

  return loop.Run();
}
