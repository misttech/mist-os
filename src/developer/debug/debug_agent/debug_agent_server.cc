// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent_server.h"

#include <zircon/errors.h>

#include "src/developer/debug/debug_agent/debug_agent.h"
#include "src/developer/debug/shared/message_loop.h"

namespace debug_agent {

void DebugAgentServer::Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) {
  FX_CHECK(debug_agent_);

  if (debug_agent_->is_connected()) {
    completer.Reply(zx::make_result(ZX_ERR_ALREADY_BOUND));
    return;
  }

  auto buffered_socket = std::make_unique<debug::BufferedZxSocket>(std::move(request.socket()));

  // Hand ownership of the socket to DebugAgent and start listening.
  debug_agent_->TakeAndConnectRemoteAPIStream(std::move(buffered_socket));

  completer.Reply(zx::make_result(ZX_OK));
}

void DebugAgentServer::OnUnboundFn(DebugAgentServer* impl, fidl::UnbindInfo info,
                                   fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end) {
  if (info.is_peer_closed()) {
    debug_agent_->Disconnect();
    debug::MessageLoop::Current()->QuitNow();
  }
  // Ignore other messages.
}

void DebugAgentServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_debugger::DebugAgent> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Unknown method: " << metadata.method_ordinal;
}

}  // namespace debug_agent
