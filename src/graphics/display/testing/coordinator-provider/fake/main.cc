// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/testing/coordinator-provider/fake/service.h"

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

  FX_LOGS(INFO) << "Starting fake fuchsia.hardware.display.Provider service.";

  std::shared_ptr<zx_device> mock_root = MockDevice::FakeRootParent();
  zx::result<> create_and_publish_service_result =
      display::FakeDisplayCoordinatorConnector::CreateAndPublishService(
          mock_root, loop.dispatcher(), outgoing);
  if (create_and_publish_service_result.is_error()) {
    FX_LOGS(ERROR) << "Cannot start display Provider server and publish service: "
                   << create_and_publish_service_result.status_string();
    return -1;
  }

  loop.Run();

  FX_LOGS(INFO) << "Quit fake Display Coordinator Connector main loop.";

  return 0;
}
