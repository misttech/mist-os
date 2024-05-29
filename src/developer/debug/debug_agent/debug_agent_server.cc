// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent_server.h"

#include <lib/fit/result.h>

#include <utility>

#include "src/developer/debug/debug_agent/debug_agent.h"
#include "src/developer/debug/ipc/records.h"
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

// Converts a FIDL filter to a debug_ipc filter or a FilterError if there was an error.
debug::Result<debug_ipc::Filter, fuchsia_debugger::FilterError> ToDebugIpcFilter(
    const fuchsia_debugger::Filter& request) {
  debug_ipc::Filter filter;

  if (request.pattern().empty()) {
    return fuchsia_debugger::FilterError::kNoPattern;
  }

  switch (request.type()) {
    case fuchsia_debugger::FilterType::kUrl:
      filter.type = debug_ipc::Filter::Type::kComponentUrl;
      break;
    case fuchsia_debugger::FilterType::kMoniker:
      filter.type = debug_ipc::Filter::Type::kComponentMoniker;
      break;
    case fuchsia_debugger::FilterType::kMonikerPrefix:
      filter.type = debug_ipc::Filter::Type::kComponentMonikerPrefix;
      break;
    case fuchsia_debugger::FilterType::kMonikerSuffix:
      filter.type = debug_ipc::Filter::Type::kComponentMonikerSuffix;
      break;
    default:
      return fuchsia_debugger::FilterError::kUnknownType;
  }

  filter.pattern = request.pattern();

  // Filters are always weak when attached via this interface.
  filter.weak = true;

  if (request.options().recursive()) {
    filter.recursive = *request.options().recursive();
  }

  return filter;
}

}  // namespace

// Static.
void DebugAgentServer::BindServer(async_dispatcher_t* dispatcher,
                                  fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end,
                                  fxl::WeakPtr<DebugAgent> debug_agent) {
  auto server = std::make_unique<DebugAgentServer>(debug_agent, dispatcher);
  auto impl_ptr = server.get();

  fidl::BindServer(dispatcher, std::move(server_end), std::move(server),
                   cpp20::bind_front(&debug_agent::DebugAgentServer::OnUnboundFn, impl_ptr));
}

DebugAgentServer::DebugAgentServer(fxl::WeakPtr<DebugAgent> agent, async_dispatcher_t* dispatcher)
    : debug_agent_(std::move(agent)), dispatcher_(dispatcher) {
  debug_agent_->AddObserver(this);
}

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

void DebugAgentServer::AttachTo(AttachToRequest& request, AttachToCompleter::Sync& completer) {
  FX_DCHECK(debug_agent_);

  auto result = AddFilter(request);
  if (result.has_error()) {
    completer.Reply(fit::error(result.err()));
    return;
  }

  auto reply = result.take_value();

  // The set removes potential duplicate koids from other filters.
  std::set<zx_koid_t> koids_to_attach;
  if (!reply.matched_processes_for_filter.empty()) {
    for (const auto& f : reply.matched_processes_for_filter) {
      koids_to_attach.insert(f.matched_pids.begin(), f.matched_pids.end());
    }
  }

  completer.Reply(fit::success(AttachToKoids({koids_to_attach.begin(), koids_to_attach.end()})));
}

DebugAgentServer::AddFilterResult DebugAgentServer::AddFilter(
    const fuchsia_debugger::Filter& fidl_filter) const {
  auto result = ToDebugIpcFilter(fidl_filter);
  if (result.has_error()) {
    return result.err();
  }

  debug_ipc::UpdateFilterRequest ipc_request;

  debug_ipc::StatusReply status;
  debug_agent_->OnStatus({}, &status);

  // OnUpdateFilter will clear all the filters before reinstalling the set that is present in the
  // IPC request, so we must be sure to copy all of the filters that were already there before
  // calling the method.
  auto agent_filters = status.filters;

  // Add in the new filter.
  agent_filters.emplace_back(result.value());
  ipc_request.filters.reserve(agent_filters.size());

  for (const auto& filter : agent_filters) {
    ipc_request.filters.push_back(filter);
  }

  debug_ipc::UpdateFilterReply reply;
  debug_agent_->OnUpdateFilter(ipc_request, &reply);

  return reply;
}

uint32_t DebugAgentServer::AttachToKoids(const std::vector<zx_koid_t>& koids) const {
  // This is not a size_t because this count is eventually fed back through a FIDL type, which
  // does not have support for size types.
  uint32_t attaches = 0;

  for (auto koid : koids) {
    debug_ipc::AttachRequest attach_request;
    attach_request.koid = koid;
    attach_request.weak = true;

    debug_ipc::AttachReply attach_reply;
    debug_agent_->OnAttach(attach_request, &attach_reply);

    // We may get an error if we're already attached to this process. DebugAgent already prints a
    // trace log for this, and it's not a problem for clients if we're already attached, so this
    // case is ignored. Other errors will produce a warning log.
    if (attach_reply.status.has_error() &&
        attach_reply.status.type() != debug::Status::Type::kAlreadyExists) {
      FX_LOGS(WARNING) << " attach to koid " << koid
                       << " failed: " << attach_reply.status.message();
    } else {
      // Normal case where we attached to something.
      attaches++;
    }
  }

  return attaches;
}

void DebugAgentServer::OnNotification(const debug_ipc::NotifyProcessStarting& notify) {
  // Ignore launching notifications.
  if (notify.type == debug_ipc::NotifyProcessStarting::Type::kLaunch) {
    return;
  }

  AttachToKoids({notify.koid});
}

void DebugAgentServer::OnUnboundFn(DebugAgentServer* impl, fidl::UnbindInfo info,
                                   fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end) {
  FX_DCHECK(debug_agent_);
  debug_agent_->RemoveObserver(this);
}

void DebugAgentServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_debugger::DebugAgent> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Unknown method: " << metadata.method_ordinal;
}

}  // namespace debug_agent
