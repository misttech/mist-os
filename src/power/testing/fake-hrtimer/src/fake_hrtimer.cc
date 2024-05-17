// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/test_base.h>
#include <fidl/fuchsia.hardware.hrtimer/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>

#include "src/power/testing/fake-hrtimer/src/device_server.h"

int main() {
  FX_LOGS(INFO) << "Start fake hrtimer component";
  async::Loop loop{&kAsyncLoopConfigAttachToCurrentThread};
  component::OutgoingDirectory outgoing(loop.dispatcher());

  auto hrtimer_device = std::make_shared<std::vector<fuchsia_hardware_hrtimer::Device>>();
  auto device_server = std::make_shared<fake_hrtimer::DeviceServer>();

  auto result = outgoing.AddUnmanagedProtocolAt<fuchsia_hardware_hrtimer::Device>(
      "hrtimer",
      [device_server](fidl::ServerEnd<fuchsia_hardware_hrtimer::Device> server_end) {
        device_server->Serve(async_get_default_dispatcher(), std::move(server_end));
      },
      "instance");
  if (result.is_error()) {
    return -1;
  }

  result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    return -1;
  }

  loop.Run();
  return 0;
}
