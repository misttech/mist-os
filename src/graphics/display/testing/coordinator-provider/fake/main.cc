// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/testing/coordinator-provider/fake/service.h"

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  trace::TraceProviderWithFdio trace_provider(loop.dispatcher());
  std::unique_ptr<sys::ComponentContext> app_context(
      sys::ComponentContext::CreateAndServeOutgoingDirectory());

  FX_LOGS(INFO) << "Starting fake fuchsia.hardware.display.Provider service.";

  std::shared_ptr<zx_device> mock_root = MockDevice::FakeRootParent();
  fake_display::ProviderService fake_display_coordinator_connector(
      std::move(mock_root), app_context.get(), loop.dispatcher());

  loop.Run();

  FX_LOGS(INFO) << "Quit fake Display Coordinator Connector main loop.";

  return 0;
}
