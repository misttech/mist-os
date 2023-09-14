// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>

#include "examples/components/pw_rpc/runner/component_runner.h"

// This runner connects to an endpoint that serves Pigweed RPC. It is assumed that the server
// implements RPC logging -- see https://pigweed.dev/pw_log_rpc.
//
// A component run by this runner is a proxy to an offloaded program. Currently, only
// Pigweed-on-Linux is supported. The `program` block contains the address and port from which the
// remote endpoint is reachable, for example:
//
// // my_pigweed_component.cml
// {
//     program: {
//         host: "2606:2800:220:1:248:1893:25c8:1946",
//         port: 33000,
//     },
//     ...
// }
//
// For an example that uses this runner, see //examples/components/pw_rpc.

int main(int argc, const char* argv[], char* envp[]) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  FX_SLOG(INFO, "Starting up.");

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());

  auto result = outgoing.AddProtocol<fuchsia_component_runner::ComponentRunner>(
      std::make_unique<ComponentRunnerImpl>(loop.dispatcher()));
  FX_CHECK(result.is_ok()) << "Failed to add runner protocol: " << result.status_string();
  result = outgoing.ServeFromStartupInfo();
  FX_CHECK(result.is_ok()) << "Failed to serve outgoing directory: " << result.status_string();

  loop.Run();
  return 0;
}
