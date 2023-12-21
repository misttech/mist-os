// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent_server.h"

#include <lib/fit/result.h>
#include <zircon/errors.h>

#include "src/developer/debug/debug_agent/debug_agent.h"
#include "src/developer/debug/shared/message_loop_fuchsia.h"

namespace debug_agent {

namespace {

// Process names are short, just 32 bytes, and fidl messages have 64k to work with. So we can
// include 2048 process names in a single message. Realistically, DebugAgent will never be attached
// to that many processes at once, so we don't need to hit the absolute limit.
constexpr size_t kMaxBatchedProcessNames = 1024;

class AttachedProcessIterator : public fidl::Server<fuchsia_debugger::AttachedProcessIterator> {
 public:
  explicit AttachedProcessIterator(fxl::WeakPtr<DebugAgent> debug_agent)
      : debug_agent_(std::move(debug_agent)) {}

  void GetNext(GetNextCompleter::Sync& completer) override {
    // First request, get the attached processes. This is unbounded, so we will always receive all
    // of the processes that DebugAgent is attached to.
    if (reply_.processes.empty()) {
      FX_CHECK(debug_agent_);

      debug_ipc::StatusRequest request;
      debug_agent_->OnStatus(request, &reply_);
      it_ = reply_.processes.begin();
    }

    std::vector<std::string> names;
    for (; it_ != reply_.processes.end() && names.size() < kMaxBatchedProcessNames; ++it_) {
      names.push_back(it_->process_name);
    }

    completer.Reply(fuchsia_debugger::AttachedProcessIteratorGetNextResponse{
        {.process_names = std::move(names)}});
  }

 private:
  fxl::WeakPtr<DebugAgent> debug_agent_;
  debug_ipc::StatusReply reply_ = {};
  std::vector<debug_ipc::ProcessRecord>::iterator it_;
};

}  // namespace

void DebugAgentServer::GetAttachedProcesses(GetAttachedProcessesRequest& request,
                                            GetAttachedProcessesCompleter::Sync& completer) {
  FX_CHECK(debug_agent_);

  // Create and bind the iterator.
  fidl::BindServer(
      debug::MessageLoopFuchsia::Current()->dispatcher(),
      fidl::ServerEnd<fuchsia_debugger::AttachedProcessIterator>(std::move(request.iterator())),
      std::make_unique<AttachedProcessIterator>(debug_agent_->GetWeakPtr()), nullptr);
}

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
                                   fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end) {}

void DebugAgentServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_debugger::DebugAgent> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Unknown method: " << metadata.method_ordinal;
}

}  // namespace debug_agent
