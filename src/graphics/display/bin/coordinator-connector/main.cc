// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include <memory>

#include "src/graphics/display/bin/coordinator-connector/devfs-factory.h"

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  trace::TraceProviderWithFdio trace_provider(loop.dispatcher());

  component::OutgoingDirectory outgoing(loop.dispatcher());

  zx::result<> serve_outgoing_directory_result = outgoing.ServeFromStartupInfo();
  if (serve_outgoing_directory_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: "
                   << serve_outgoing_directory_result.status_string();
    return -1;
  }

  FX_LOGS(INFO) << "Starting standalone fuchsia.hardware.display.Provider service.";

  zx::result<> create_and_publish_service_result =
      display::DevFsCoordinatorFactory::CreateAndPublishService(outgoing, loop.dispatcher());
  if (create_and_publish_service_result.is_error()) {
    FX_LOGS(ERROR) << "Cannot start display Provider server and publish service: "
                   << create_and_publish_service_result.status_string();
    return -1;
  }

  loop.Run();

  FX_LOGS(INFO) << "Quit Display Coordinator Connector main loop.";

  return 0;
}
