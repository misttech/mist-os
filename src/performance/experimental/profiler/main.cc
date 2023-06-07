// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <stdlib.h>
#include <unistd.h>

#include "profiler_controller_impl.h"

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();

  // Expose the FIDL server.
  component::OutgoingDirectory outgoing = component::OutgoingDirectory(dispatcher);
  zx::result result = outgoing.AddUnmanagedProtocol<fuchsia_cpu_profiler::Session>(
      [dispatcher](fidl::ServerEnd<fuchsia_cpu_profiler::Session> server_end) {
        fidl::BindServer(dispatcher, std::move(server_end),
                         std::make_unique<ProfilerControllerImpl>(dispatcher),
                         std::mem_fn(&ProfilerControllerImpl::OnUnbound));
      });
  FX_CHECK(result.is_ok()) << "Failed to expose ProfilingController protocol: "
                           << result.status_string();

  result = outgoing.ServeFromStartupInfo();
  FX_CHECK(result.is_ok()) << "Failed to serve outgoing directory: " << result.status_string();
  return loop.Run() == ZX_OK;
}
