// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent_server.h"

#include <lib/fit/result.h>

#include <utility>

#include "fidl/fuchsia.debugger/cpp/natural_types.h"
#include "src/developer/debug/debug_agent/backtrace_utils.h"
#include "src/developer/debug/debug_agent/component_manager.h"
#include "src/developer/debug/debug_agent/debug_agent.h"
#include "src/developer/debug/debug_agent/debugged_process.h"
#include "src/developer/debug/debug_agent/debugged_thread.h"
#include "src/developer/debug/debug_agent/minidump_iterator.h"
#include "src/developer/debug/debug_agent/process_info_iterator.h"
#include "src/developer/debug/debug_agent/system_interface.h"
#include "src/developer/debug/ipc/filter_utils.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/developer/debug/ipc/records.h"

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

// Converts a FIDL filter to a debug_ipc filter or a FilterError if there was an error. |id| is
// optional because sometimes callers want to construct a debug_ipc Filter without the intention of
// installing it to DebugAgent. If |id| is not given and the filter is installed, it will be
// impossible to derive the correct attach configuration from the filter.
debug::Result<debug_ipc::Filter, fuchsia_debugger::FilterError> ToDebugIpcFilter(
    const fuchsia_debugger::Filter& request, std::optional<uint32_t> id) {
  debug_ipc::Filter filter;

  // Set the highest bit to 1 to differentiate filters set via this interface from filters set in
  // the frontend. There's no communication between this interface and another debug_ipc client to
  // this same DebugAgent, so coordination is difficult.
  constexpr uint32_t kFilterIdBase = 1 << 31;

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
  if (id) {
    filter.id = kFilterIdBase | *id;
  }

  if (!request.options().job_only()) {
    // Non-job-only filters are always weak when attached via this interface.
    filter.config.weak = true;
  } else {
    // Meanwhile, job-only filters are always strong (the default) when attached via this interface.
    filter.config.job_only = *request.options().job_only();
  }

  if (request.options().recursive()) {
    if (filter.config.job_only) {
      return fuchsia_debugger::FilterError::kInvalidOptions;
    }
    filter.config.recursive = *request.options().recursive();
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

  impl_ptr->binding_ref_ =
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
      dispatcher_,
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

  completer.Reply(fit::success(AttachToFilterMatches(reply.matched_processes_for_filter)));
}

DebugAgentServer::AddFilterResult DebugAgentServer::AddFilter(
    const fuchsia_debugger::Filter& fidl_filter) {
  auto result = ToDebugIpcFilter(fidl_filter, next_filter_id_++);
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
  // Save the filter ourselves as well so that we can use the filter configuration to derive the
  // attach configuration when there's a match.
  filters_[agent_filters.back().id] = agent_filters.back();

  for (const auto& filter : agent_filters) {
    ipc_request.filters.push_back(filter);
  }

  debug_ipc::UpdateFilterReply reply;
  debug_agent_->OnUpdateFilter(ipc_request, &reply);

  return reply;
}

uint32_t DebugAgentServer::AttachToFilterMatches(
    const std::vector<debug_ipc::FilterMatch>& filter_matches) const {
  // This is not a size_t because this count is eventually fed back through a FIDL type, which
  // does not have support for size types.
  uint32_t attaches = 0;

  std::vector<debug_ipc::Filter> ipc_filters(filters_.size());
  for (const auto& [_id, filter] : filters_) {
    ipc_filters.push_back(filter);
  }

  auto pids_to_attach = debug_ipc::GetAttachConfigsForFilterMatches(filter_matches, ipc_filters);

  for (const auto& [koid, attach_config] : pids_to_attach) {
    debug_ipc::AttachRequest request;
    request.koid = koid;
    request.config = attach_config;

    debug_ipc::AttachReply reply;
    debug_agent_->OnAttach(request, &reply);

    // We may get an error if we're already attached to this process, or in the case of job-only,
    // attached to an ancestor. DebugAgent already prints a trace log for this, and it's not a
    // problem for clients if we're already attached to what they care about, so this case is
    // ignored. Other errors will produce a warning log.
    if (reply.status.has_error() && reply.status.type() != debug::Status::Type::kAlreadyExists) {
      FX_LOGS(WARNING) << " attach to koid " << koid << " failed: " << reply.status.message();
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

  // We only get process starting notifications (as a debug_ipc client) when a filter matches. We
  // also only get this notification for processes specifically, so the koid is always a process.
  // When we matched a job_only filter, we create a DebuggedProcess object for it internally, and
  // don't need an explicit attach.
  if (!debug_agent_->GetDebuggedProcess(notify.koid)) {
    AttachToFilterMatches({{notify.filter_id, {notify.koid}}});
  }
}

void DebugAgentServer::OnNotification(const debug_ipc::NotifyException& notify) {
  // We always destruct ourselves whenever the client hangs up.
  FX_DCHECK(binding_ref_);

  if (debug_ipc::IsDebug(notify.type)) {
    // Not the kind of exception that our clients are interested in.
    return;
  }

  // The thread is in an exception, we don't need to suspend it, but we do need
  // to resume it when we're done (if there isn't a debug_ipc client).
  auto thread = debug_agent_->GetDebuggedThread(notify.thread.id);

  fuchsia_debugger::DebugAgentOnFatalExceptionRequest event;

  event.thread(notify.thread.id.thread);
  event.backtrace(
      GetBacktraceMarkupForThread(thread->process()->process_handle(), thread->thread_handle()));

  fit::result result = fidl::SendEvent(*binding_ref_)->OnFatalException(event);
  if (!result.is_ok()) {
    FX_LOGS(WARNING) << "Error sending event: " << result.error_value();
  }

  // Asynchronously detach from the process so the system can handle the exception as normal if
  // there is no debug_ipc client. This must be asynchronous so that the low level exception handler
  // doesn't have the process removed out from under it when we should be synchronously handling the
  // exception.
  //
  // |this| is owned by the async dispatcher associated with this message loop, so it's safe to
  // capture. Similarly, DebugAgent is allocated in main, so our reference should also always be
  // valid here. The thread and process might exit independently before the message loop runs this
  // callback, so we capture the process's koid by value first.
  debug::MessageLoop::Current()->PostTask(
      FROM_HERE, [=, process_koid = thread->process()->koid()]() {
        FX_DCHECK(this);
        FX_DCHECK(debug_agent_);

        // The check for the DebuggedProcess isn't strictly necessary, but prevents some log spam
        // in the case that the process has already gone away after the exception was released. We
        // want the log to be forwarded to debug_ipc clients so we'll check here to avoid the error
        // case when this is more likely to happen.
        //
        // TODO(https://fxbug.dev/377671670): Write better tests for this.
        if (!debug_agent_->is_connected() && debug_agent_->GetDebuggedProcess(process_koid)) {
          debug_ipc::DetachRequest request;
          request.koid = process_koid;
          debug_ipc::DetachReply reply;
          debug_agent_->OnDetach(request, &reply);

          if (reply.status.has_error()) {
            FX_LOGS(WARNING) << "Failed to detach from process " << process_koid << ": "
                             << reply.status.message();
          }
        }
      });
}

DebugAgentServer::GetMatchingProcessesResult DebugAgentServer::GetMatchingProcesses(
    std::optional<fuchsia_debugger::Filter> filter) const {
  FX_DCHECK(debug_agent_);

  std::vector<DebuggedProcess*> processes;
  const auto& attached_processes = debug_agent_->GetAllProcesses();

  if (filter) {
    // We're not installing this filter, so don't need to provide a filter id.
    auto result = ToDebugIpcFilter(*filter, std::nullopt);
    if (result.has_error()) {
      return result.err();
    }

    const auto& component_manager = debug_agent_->system_interface().GetComponentManager();

    for (const auto& [_koid, process] : attached_processes) {
      const auto& components = component_manager.FindComponentInfo(process->process_handle());

      if (debug_ipc::FilterMatches(result.value(), process->process_handle().GetName(),
                                   components)) {
        processes.push_back(process.get());
      }
    }
  } else {
    for (const auto& [_koid, process] : attached_processes) {
      processes.push_back(process.get());
    }
  }

  return processes;
}

void DebugAgentServer::GetProcessInfo(GetProcessInfoRequest& request,
                                      GetProcessInfoCompleter::Sync& completer) {
  FX_DCHECK(debug_agent_);

  auto result = GetMatchingProcesses(request.options().filter());
  if (result.has_error()) {
    completer.Reply(fit::error(result.err()));
    return;
  }

  // At this point it is invalid to have either filtered out all of the attached processes (or be
  // attached to nothing). The first GetNext call on the iterator will produce this error, which is
  // more appropriate than for this method.
  fidl::BindServer(
      dispatcher_,
      fidl::ServerEnd<fuchsia_debugger::ProcessInfoIterator>(std::move(request.iterator())),
      std::make_unique<ProcessInfoIterator>(debug_agent_, result.take_value(),
                                            std::move(request.options().interest())));

  completer.Reply(fit::success());
}

void DebugAgentServer::GetMinidumps(GetMinidumpsRequest& request,
                                    GetMinidumpsCompleter::Sync& completer) {
  auto result = GetMatchingProcesses(request.options().filter());
  if (result.has_error()) {
    completer.Reply(fit::error(result.err()));
    return;
  }

  fidl::BindServer(
      dispatcher_,
      fidl::ServerEnd<fuchsia_debugger::MinidumpIterator>(std::move(request.iterator())),
      std::make_unique<MinidumpIterator>(debug_agent_, result.take_value()));

  completer.Reply(fit::success());
}

void DebugAgentServer::OnUnboundFn(DebugAgentServer* impl, fidl::UnbindInfo info,
                                   fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end) {
  // DebugAgent will be destructed before the server bound to the outgoing directory, so it is
  // possible for DebugAgent to be null here.
  if (!debug_agent_)
    return;

  debug_agent_->RemoveObserver(this);
}

void DebugAgentServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_debugger::DebugAgent> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Unknown method: " << metadata.method_ordinal;
}

}  // namespace debug_agent
