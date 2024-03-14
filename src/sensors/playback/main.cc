// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

#include "src/sensors/playback/driver_impl.h"
#include "src/sensors/playback/playback_controller.h"
#include "src/sensors/playback/playback_impl.h"

using sensors::playback::DriverImpl;
using sensors::playback::PlaybackController;
using sensors::playback::PlaybackImpl;

int main(int argc, char* argv[]) {
  FX_LOGS(INFO) << "Playback service starting.";

  async::Loop fidl_loop(&kAsyncLoopConfigAttachToCurrentThread);
  async_dispatcher_t* fidl_dispatcher = fidl_loop.dispatcher();

  component::OutgoingDirectory outgoing(fidl_dispatcher);

  // The controller runs on a separate thread from all the FIDL interactions with clients.
  async::Loop controller_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  PlaybackController controller(controller_loop.dispatcher());

  zx_status_t start_result = controller_loop.StartThread("PlaybackController Thread");
  if (start_result != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to start PlaybackController thread.";
    return -1;
  }

  zx::result result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << result.status_string();
    return -1;
  }

  result = outgoing.AddUnmanagedProtocol<fuchsia_hardware_sensors::Playback>(
      [fidl_dispatcher,
       &controller](fidl::ServerEnd<fuchsia_hardware_sensors::Playback> server_end) {
        FX_LOGS(INFO) << "Incoming connection for "
                      << fidl::DiscoverableProtocolName<fuchsia_hardware_sensors::Playback>;
        PlaybackImpl::BindSelfManagedServer(fidl_dispatcher, controller, std::move(server_end));
      });
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Playback protocol: " << result.status_string();
    return -1;
  }

  result = outgoing.AddUnmanagedProtocol<fuchsia_hardware_sensors::Driver>(
      [fidl_dispatcher, &controller](fidl::ServerEnd<fuchsia_hardware_sensors::Driver> server_end) {
        FX_LOGS(INFO) << "Incoming connection for "
                      << fidl::DiscoverableProtocolName<fuchsia_hardware_sensors::Driver>;
        DriverImpl::BindSelfManagedServer(fidl_dispatcher, controller, std::move(server_end));
      });
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Driver protocol: " << result.status_string();
    return -1;
  }

  fidl_loop.Run();

  controller_loop.Quit();
  controller_loop.JoinThreads();

  FX_LOGS(INFO) << "Playback service exiting.";
  return EXIT_SUCCESS;
}
