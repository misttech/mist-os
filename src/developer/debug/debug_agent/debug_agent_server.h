// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_SERVER_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_SERVER_H_

#include <fidl/fuchsia.debugger/cpp/fidl.h>

#include "src/developer/debug/debug_agent/debug_agent_observer.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/developer/debug/shared/result.h"
#include "src/lib/fxl/memory/weak_ptr.h"

namespace debug_agent {

class DebugAgent;

class DebugAgentServer : public fidl::Server<fuchsia_debugger::DebugAgent>,
                         public DebugAgentObserver {
 public:
  static void BindServer(async_dispatcher_t* dispatcher,
                         fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end,
                         fxl::WeakPtr<DebugAgent> debug_agent);

  DebugAgentServer(fxl::WeakPtr<DebugAgent> agent, async_dispatcher_t* dispatcher);

  // fidl::Server<fuchsia_debugger::DebugAgent> implementation.
  void GetAttachedProcesses(GetAttachedProcessesRequest& request,
                            GetAttachedProcessesCompleter::Sync& completer) override;
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override;
  void AttachTo(AttachToRequest& request, AttachToCompleter::Sync& completer) override;

  void OnUnboundFn(DebugAgentServer* impl, fidl::UnbindInfo info,
                   fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end);
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_debugger::DebugAgent> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  // DebugAgentObserver implementation. For right now we only care about process starting
  // notifications.
  void OnNotification(const debug_ipc::NotifyProcessStarting& notify) override;

 private:
  // The test fixture is a friend so that we can access the private methods that are doing all the
  // work directly without having to mock out the FIDL primitives.
  friend class DebugAgentServerTest;

  using AddFilterResult =
      debug::Result<debug_ipc::UpdateFilterReply, fuchsia_debugger::FilterError>;

  // Convert |fidl_filter| to a debug_ipc equivalent, then add that to DebugAgent's set of filters.
  AddFilterResult AddFilter(const fuchsia_debugger::Filter& fidl_filter) const;

  // Attempt to attach to all given koids, some may fail, which is not reported as an error. The
  // number of successful attaches is returned.
  uint32_t AttachToKoids(const std::vector<zx_koid_t>& koids) const;

  fxl::WeakPtr<DebugAgent> debug_agent_;
  async_dispatcher_t* dispatcher_;
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_SERVER_H_
