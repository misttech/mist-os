// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This simple program listens on the fuchsia::debugger::DebugAgent protocol,
// and launch a debug_agent when there's a connect request. The debug_agent
// launched expects a numbered handle at PA_HND(PA_USER0, 0), which should
// point to a zx::socket object.

#include <fidl/fuchsia.debugger/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/spawn.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/process.h>
#include <zircon/processargs.h>

namespace {

class DebugAgentLauncher : public fidl::Server<fuchsia_debugger::DebugAgent> {
 public:
  // Launch debug_agent on connect, passing the socket as a numbered handle.
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    FX_LOGS(DEBUG) << "Spawning debug_agent...";
    const char* path = "/pkg/bin/debug_agent";
    const char* argv[] = {path, "--channel-mode", nullptr};
    fdio_spawn_action_t action = {
        .action = FDIO_SPAWN_ACTION_ADD_HANDLE,
        .h =
            {
                // Must correspond to main.cc.
                .id = PA_HND(PA_USER0, 0),
                .handle = request.socket().release(),
            },
    };
    zx::process process;
    char err_msg[FDIO_SPAWN_ERR_MSG_MAX_LENGTH] = {};
    zx_status_t status =
        fdio_spawn_etc(zx_job_default(), FDIO_SPAWN_CLONE_ALL, path, argv,
                       /*environ=*/nullptr, 1, &action, process.reset_and_get_address(), err_msg);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to launch debug_agent: " << err_msg;
    }
    completer.Reply(status);
  }
};

}  // namespace

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  fuchsia_logging::SetLogSettings(fuchsia_logging::LogSettings{});

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());
  zx::result res = outgoing.ServeFromStartupInfo();
  if (res.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << res.status_string();
    return -1;
  }

  res = outgoing.AddProtocol<fuchsia_debugger::DebugAgent>(std::make_unique<DebugAgentLauncher>());
  if (res.is_error()) {
    FX_LOGS(ERROR) << "Failed to add DebugAgent protocol: " << res.status_string();
    return -1;
  }

  FX_LOGS(INFO) << "Start listening on FIDL fuchsia::debugger::DebugAgent.";
  return loop.Run();
}
