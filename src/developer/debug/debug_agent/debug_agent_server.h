// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_SERVER_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_SERVER_H_

#include <fidl/fuchsia.debugger/cpp/fidl.h>

#include "src/lib/fxl/memory/weak_ptr.h"

namespace debug_agent {

class DebugAgent;

class DebugAgentServer : public fidl::Server<fuchsia_debugger::DebugAgent> {
 public:
  explicit DebugAgentServer(fxl::WeakPtr<DebugAgent> agent) : debug_agent_(std::move(agent)) {}

  void GetAttachedProcesses(GetAttachedProcessesRequest& request,
                            GetAttachedProcessesCompleter::Sync& completer) override;
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override;
  void OnUnboundFn(DebugAgentServer* impl, fidl::UnbindInfo info,
                   fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end);
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_debugger::DebugAgent> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  fxl::WeakPtr<DebugAgent> debug_agent_;
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_SERVER_H_
