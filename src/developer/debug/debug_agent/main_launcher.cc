// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This simple program listens on the fuchsia::debugger::DebugAgent protocol,
// and launch a debug_agent when there's a connect request. The debug_agent
// launched expects a numbered handle at PA_HND(PA_USER0, 0), which should
// point to a zx::channel object, which is the server_end of the
// fuchshia_debugger::DebugAgent protocol. This server_end is bound to the
// DebugAgent's MessageLoop from main.

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.debugger/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/spawn.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <lib/zx/process.h>
#include <zircon/errors.h>
#include <zircon/processargs.h>

#include <map>

#include "src/developer/debug/debug_agent/launcher.h"

namespace {

zx_status_t LaunchDebugAgent(fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end) {
  const char* path = "/pkg/bin/debug_agent";
  const char* argv[] = {path, nullptr};

  FX_LOGS(DEBUG) << "Spawning debug_agent...";

  // Hand the other end of the channel to DebugAgent.
  fdio_spawn_action_t action = {
      .action = FDIO_SPAWN_ACTION_ADD_HANDLE,
      .h =
          {
              .id = PA_HND(PA_USER0, 0),
              .handle = server_end.TakeHandle().release(),
          },
  };

  char err_msg[FDIO_SPAWN_ERR_MSG_MAX_LENGTH] = {};
  zx_status_t status = fdio_spawn_etc(zx_job_default(), FDIO_SPAWN_CLONE_ALL, path, argv,
                                      /*environ=*/nullptr, 1, &action, nullptr, err_msg);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to launch debug_agent: " << err_msg;
  }

  return status;
}

// To support a soft transition to the Launcher protocol, this class implements the |DebugAgent|
// protocol, but is also a client of the actual server in DebugAgentServer. This launcher will keep
// the client end of the channel open as long as the DebugAgent is alive.
// TODO(b/311409740): Complete the soft transition and delete this.
class DebugAgentLegacyLauncher : public fidl::Server<fuchsia_debugger::DebugAgent> {
  // Manages a single instance of a DebugAgent protocol client. At construction time, this class
  // takes over the client_end of the protocol from the caller, registers itself for client events,
  // and calls the |Connect| method. The provided callbacks will be called to handle the response to
  // the |Connect| method and another to cleanup when a ZX_CHANNEL_PEER_CLOSED signal is received.
  class DebugAgentClient : public fidl::AsyncEventHandler<fuchsia_debugger::DebugAgent> {
   public:
    DebugAgentClient(fidl::ClientEnd<fuchsia_debugger::DebugAgent> client_end, zx::socket socket,
                     fit::callback<void(zx_status_t)> on_connect_complete,
                     fit::callback<void(zx_handle_t)> on_peer_closed)
        : client_handle_(client_end.handle()->get()), on_peer_closed_(std::move(on_peer_closed)) {
      client_.Bind(std::move(client_end), async_get_default_dispatcher(), this);
      client_->Connect(fuchsia_debugger::DebugAgentConnectRequest(std::move(socket)))
          .Then([cb = std::move(on_connect_complete)](
                    fidl::Result<fuchsia_debugger::DebugAgent::Connect>& result) mutable {
            if (result.is_ok()) {
              return cb(ZX_OK);
            } else if (result.error_value().is_framework_error()) {
              return cb(result.error_value().framework_error().status());
            } else {
              return cb(result.error_value().domain_error());
            }
          });
    }

    void on_fidl_error(fidl::UnbindInfo info) override {
      if (info.is_peer_closed()) {
        // When the server closes the channel report ourselves as ready to be destructed.
        on_peer_closed_(client_handle_);
      }
    }

    void handle_unknown_event(
        fidl::UnknownEventMetadata<fuchsia_debugger::DebugAgent> metadata) override {
      FX_LOGS(WARNING) << "Unknown event: " << metadata.event_ordinal;
    }

   private:
    zx_handle_t client_handle_;
    fit::callback<void(zx_handle_t)> on_peer_closed_;
    fidl::Client<fuchsia_debugger::DebugAgent> client_;
  };

  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    FX_CHECK(request.socket().is_valid());

    // Create the client and server ends of the DebugAgent protocol. We will be injecting ourselves
    // as the "client" in this case. We must keep the client end open as long as the DebugAgent is
    // actively connected to the zx::socket.
    auto [client_end, server_end] =
        std::move(fidl::CreateEndpoints<fuchsia_debugger::DebugAgent>().value());

    zx_status_t status = LaunchDebugAgent(std::move(server_end));
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to launch DebugAgent";
      return;
    }

    // Now we bind ourselves to the client end and send over the zx::socket.
    zx_handle_t client_handle = client_end.handle()->get();
    auto client = std::make_unique<DebugAgentClient>(
        std::move(client_end), std::move(request.socket()),
        [completer = completer.ToAsync()](zx_status_t status) mutable {
          completer.Reply(zx::make_result(status));
          return;
        },
        // It is safe to capture |this| because |this| owns the client and the
        // dispatcher will only dispatch events on this thread.
        [this](zx_handle_t handle) { clients_.erase(handle); });

    clients_.insert({client_handle, std::move(client)});
  }

  // Unsupported methods for the legacy launcher.
  void GetAttachedProcesses(GetAttachedProcessesRequest& request,
                            GetAttachedProcessesCompleter::Sync& completer) override {}

  void AttachTo(AttachToRequest& request, AttachToCompleter::Sync& completer) override {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_debugger::DebugAgent> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_LOGS(WARNING) << "Unknown method: " << metadata.method_ordinal;
  }

 private:
  // This ensures that every client that is created is kept alive until it
  // calls the |on_peer_closed| function that we supply to it. The callback
  // takes the handle that was associated with that client and removes it from
  // the map. The DebugAgentClient will be responsible for unbinding their side
  // of the channel.
  std::map<zx_handle_t, std::unique_ptr<DebugAgentClient>> clients_;
};
}  // namespace

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  fuchsia_logging::SetLogSettings(fuchsia_logging::LogSettings{});

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());

  zx::result res =
      outgoing.AddProtocol<fuchsia_debugger::Launcher>(std::make_unique<DebugAgentLauncher>());
  if (res.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Launcher protocol: " << res.status_string();
    return -1;
  }

  res = outgoing.AddProtocol<fuchsia_debugger::DebugAgent>(
      std::make_unique<DebugAgentLegacyLauncher>());
  if (res.is_error()) {
    FX_LOGS(ERROR) << "Failed to add DebugAgent protocol: " << res.status_string();
    return -1;
  }

  res = outgoing.ServeFromStartupInfo();
  if (res.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << res.status_string();
    return -1;
  }

  FX_LOGS(INFO) << "Start listening on FIDL fuchsia::debugger::Launcher.";
  return loop.Run();
}
