// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent.h"

#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/features.h>
#include <zircon/status.h>
#include <zircon/syscalls/debug.h>
#include <zircon/syscalls/exception.h>
#include <zircon/types.h>

#include <algorithm>
#include <memory>

#include "src/developer/debug/debug_agent/arch.h"
#include "src/developer/debug/debug_agent/binary_launcher.h"
#include "src/developer/debug/debug_agent/component_manager.h"
#include "src/developer/debug/debug_agent/debugged_job.h"
#include "src/developer/debug/debug_agent/debugged_process.h"
#include "src/developer/debug/debug_agent/debugged_thread.h"
#include "src/developer/debug/debug_agent/exception_handle.h"
#include "src/developer/debug/debug_agent/job_handle.h"
#include "src/developer/debug/debug_agent/process_breakpoint.h"
#include "src/developer/debug/debug_agent/system_interface.h"
#include "src/developer/debug/debug_agent/time.h"
#include "src/developer/debug/ipc/filter_utils.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/developer/debug/ipc/records.h"
#include "src/developer/debug/shared/logging/logging.h"
#include "src/developer/debug/shared/status.h"

#if defined(__linux__)
#include <unistd.h>
#endif

namespace debug_agent {

namespace {

constexpr size_t kMegabyte = 1024 * 1024;

std::string LogResumeRequest(const debug_ipc::ResumeRequest& request) {
  std::stringstream ss;
  ss << "Got resume request for ";

  // Print thread koids.
  if (request.ids.empty()) {
    ss << "all processes.";
  } else {
    for (size_t i = 0; i < request.ids.size(); i++) {
      if (i > 0)
        ss << ", ";
      ss << "(" << request.ids[i].process << ", " << request.ids[i].thread << ")";
    }
  }

  // Print step range.
  if (request.range_begin != request.range_end)
    ss << ", Range: [" << std::hex << request.range_begin << ", " << request.range_end << "]";

  return ss.str();
}

bool ShouldDeferSendingModules(const debug_ipc::AttachConfig& config) {
  // Attaching to a job should always defer modules, regardless of |weak|.
  return config.weak || config.target == debug_ipc::AttachConfig::Target::kJob;
}

}  // namespace

DebugAgent::DebugAgent(std::unique_ptr<SystemInterface> system_interface)
    : adapter_(std::make_unique<RemoteAPIAdapter>(this, nullptr)),
      system_interface_(std::move(system_interface)),
      weak_factory_(this) {
  // Register ourselves to receive component events and limbo events.
  //
  // It's safe to pass |this| here because |this| owns |system_interface|, which owns
  // |ComponentManager| and |LimboProvider|.
  system_interface_->GetComponentManager().SetDebugAgent(this);
  system_interface_->GetLimboProvider().set_on_enter_limbo(
      [this](const LimboProvider::Record& record) { OnProcessEnteredLimbo(record); });

#ifdef __Fuchsia__
  // Watch the root job.
  auto status = AttachToRootJob();
  if (status.has_error()) {
    LOGS(Error) << "Failed to watch the root job: " << status.message();
  }
#endif  // __Fuchsia__
}

fxl::WeakPtr<DebugAgent> DebugAgent::GetWeakPtr() { return weak_factory_.GetWeakPtr(); }

void DebugAgent::TakeAndConnectRemoteAPIStream(std::unique_ptr<debug::BufferedStream> stream) {
  // Now we can create a BufferedZxSocket to pass to the RemoteAPIAdapter. The data path is:
  //
  // BufferedZxSocket -> RemoteAPIAdapter -> DebugAgent
  //
  // DebugAgent owns both the BufferedZxSocket and RemoteAPIAdapter, since the DebugAgent can be
  // started without a socket connection to a host tool, so it is safe to capture |this|. When the
  // socket is closed, we exit.
  adapter_->set_stream(&stream->stream());
  stream->set_data_available_callback([this]() { adapter_->OnStreamReadable(); });
  stream->set_error_callback([this]() {
    // Unconditionally quit when the debug_ipc socket is closed.
    Disconnect();
    ClearState();
    debug::MessageLoop::Current()->QuitNow();
  });

  // Start listening.
  Connect(std::move(stream));
}

void DebugAgent::Connect(std::unique_ptr<debug::BufferedStream> stream) {
  FX_DCHECK(stream) << "Cannot connect to an invalid stream!";

  buffered_stream_ = std::move(stream);

  FX_CHECK(buffered_stream_->Start()) << "Failed to connect to the FIDL socket";
}

void DebugAgent::Disconnect() {
  // Can only disconnect from a connected state.
  FX_DCHECK(buffered_stream_);

  // Release all resources associated with the previous connection.
  buffered_stream_->Reset();
}

void DebugAgent::ClearState() {
  // Reset debugging State
  debug::LogBackend::Unset();

  // Detach from all jobs first so we stop getting process starting notifications.
  jobs_.clear();

  // Removes breakpoints before we detach from the processes, although it should also be safe
  // to reverse the order.
  breakpoints_.clear();
  // Detach us from all processes.
  procs_.clear();
}

void DebugAgent::RemoveDebuggedProcess(zx_koid_t process_koid) {
  auto found = procs_.find(process_koid);
  if (found == procs_.end())
    FX_NOTREACHED();
  else
    procs_.erase(found);
}

Breakpoint* DebugAgent::GetBreakpoint(uint32_t breakpoint_id) {
  if (auto found = breakpoints_.find(breakpoint_id); found != breakpoints_.end())
    return &found->second;
  return nullptr;
}

void DebugAgent::RemoveBreakpoint(uint32_t breakpoint_id) {
  if (auto found = breakpoints_.find(breakpoint_id); found != breakpoints_.end())
    breakpoints_.erase(found);
}

void DebugAgent::OnHello(const debug_ipc::HelloRequest& request, debug_ipc::HelloReply* reply) {
  if (request.version >= debug_ipc::kMinimumProtocolVersion &&
      request.version <= debug_ipc::kCurrentProtocolVersion) {
    // Downgrade only when the requested version is supported by us.
    ipc_version_ = request.version;
  } else {
    LOGS(Error) << "Unsupported IPC version: " << request.version << ", supported range is "
                << debug_ipc::kMinimumProtocolVersion << "-" << debug_ipc::kCurrentProtocolVersion;
    ipc_version_ = debug_ipc::kCurrentProtocolVersion;
  }

  // Signature is default-initialized.
  reply->version = ipc_version_;
  reply->arch = arch::GetCurrentArch();
  reply->platform = debug::CurrentSystemPlatform();

#if defined(__Fuchsia__)
  reply->page_size = zx_system_get_page_size();
#elif defined(__linux__)
  reply->page_size = getpagesize();
#else
#error Need platform page size.
#endif

  // Only enable log backend after the handshake is finished.
  debug::LogBackend::Set(this, true);
}

void DebugAgent::OnStatus(const debug_ipc::StatusRequest& request, debug_ipc::StatusReply* reply) {
  // Get the attached processes.
  reply->processes.reserve(procs_.size());
  for (auto& [process_koid, proc] : procs_) {
    debug_ipc::ProcessRecord process_record = {};
    process_record.process_koid = process_koid;
    process_record.process_name = proc->process_handle().GetName();

    process_record.components =
        system_interface_->GetComponentManager().FindComponentInfo(proc->process_handle());

    auto threads = proc->GetThreads();
    process_record.threads.reserve(threads.size());
    for (auto* thread : threads) {
      process_record.threads.emplace_back(
          thread->GetThreadRecord(debug_ipc::ThreadRecord::StackAmount::kMinimal));
    }

    reply->processes.emplace_back(std::move(process_record));
  }

  reply->breakpoints.reserve(breakpoints_.size());
  for (auto& [_, bp] : breakpoints_) {
    reply->breakpoints.push_back(bp.settings());
  }

  reply->filters.reserve(filters_.size());
  for (auto& filter : filters_) {
    reply->filters.push_back(filter.filter());
  }

  // Get the limbo processes.
  if (system_interface_->GetLimboProvider().Valid()) {
    for (const auto& [process_koid, record] :
         system_interface_->GetLimboProvider().GetLimboRecords()) {
      debug_ipc::ProcessRecord process_record = {};
      process_record.process_koid = process_koid;
      process_record.process_name = record.process->GetName();

      process_record.components =
          system_interface_->GetComponentManager().FindComponentInfo(*record.process);

      // For now, only fill the thread blocked on exception.
      process_record.threads.push_back(record.thread->GetThreadRecord(process_koid));

      reply->limbo.push_back(std::move(process_record));
    }
  }
}

void DebugAgent::OnRunBinary(const debug_ipc::RunBinaryRequest& request,
                             debug_ipc::RunBinaryReply* reply) {
  reply->timestamp = GetNowTimestamp();
  if (request.argv.empty()) {
    reply->status = debug::Status("No launch arguments provided");
    return;
  }

  LaunchProcess(request, reply);
}

void DebugAgent::OnRunComponent(const debug_ipc::RunComponentRequest& request,
                                debug_ipc::RunComponentReply* reply) {
  reply->status = system_interface_->GetComponentManager().LaunchComponent(request.url);
}

void DebugAgent::OnRunTest(const debug_ipc::RunTestRequest& request,
                           debug_ipc::RunTestReply* reply) {
  reply->status = system_interface_->GetComponentManager().LaunchTest(request.url, request.realm,
                                                                      request.case_filters);
}

void DebugAgent::OnKill(const debug_ipc::KillRequest& request, debug_ipc::KillReply* reply) {
  reply->timestamp = GetNowTimestamp();
  // See first if the process is in limbo.
  LimboProvider& limbo = system_interface_->GetLimboProvider();
  if (limbo.Valid() && limbo.IsProcessInLimbo(request.process_koid)) {
    // Release if from limbo, which will effectivelly kill it.
    reply->status = limbo.ReleaseProcess(request.process_koid);
    return;
  }

  // Otherwise search locally.
  auto debug_process = GetDebuggedProcess(request.process_koid);
  if (!debug_process) {
    reply->status = debug::Status("Process is not currently being debugged.");
    return;
  }

  debug_process->OnKill(request, reply);

  // Check if this was a limbo "kill". If so, mark this process to be removed from limbo when it
  // re-enters it and tell the client that we successfully killed it.
  if (reply->status.has_error() && debug_process->from_limbo()) {
    killed_limbo_procs_.insert(debug_process->koid());
    reply->status = debug::Status();
  }

  RemoveDebuggedProcess(request.process_koid);
}

void DebugAgent::OnDetach(const debug_ipc::DetachRequest& request, debug_ipc::DetachReply* reply) {
  reply->timestamp = GetNowTimestamp();

  // First check if the process is waiting in limbo. If so, release it.
  LimboProvider& limbo = system_interface_->GetLimboProvider();
  if (limbo.Valid() && limbo.IsProcessInLimbo(request.koid)) {
    reply->status = limbo.ReleaseProcess(request.koid);
    return;
  }

  auto debug_process = GetDebuggedProcess(request.koid);
  if (debug_process) {
    RemoveDebuggedProcess(request.koid);
    reply->status = debug::Status();
    return;
  }

  auto debugged_job = GetDebuggedJob(request.koid);
  if (debugged_job) {
    jobs_.erase(request.koid);
    reply->status = debug::Status();
    return;
  }

  reply->status = debug::Status("Not currently attached to " + std::to_string(request.koid) +
                                " to detach from.");
}

void DebugAgent::OnPause(const debug_ipc::PauseRequest& request, debug_ipc::PauseReply* reply) {
  std::vector<debug_ipc::ProcessThreadId> paused;
  DEBUG_LOG(Agent) << "Got Pause request";

  if (request.ids.empty()) {
    // Pause everything.
    paused = ClientSuspendAll();
  } else {
    // Pause specific threads.
    for (const debug_ipc::ProcessThreadId& id : request.ids) {
      if (DebuggedProcess* proc = GetDebuggedProcess(id.process)) {
        if (id.thread) {
          // Single thread in that process.
          if (DebuggedThread* thread = proc->GetThread(id.thread)) {
            thread->ClientSuspend(true);
            paused.push_back(id);
          } else {
            LOGS(Warn) << "Could not find thread by koid: " << id.thread;
          }
        } else {
          // All threads in the process.
          std::vector<debug_ipc::ProcessThreadId> proc_threads = proc->ClientSuspendAllThreads();
          paused.insert(paused.end(), proc_threads.begin(), proc_threads.end());
        }
      }
    }
  }

  // Save the affected thread info.
  for (const debug_ipc::ProcessThreadId& id : paused) {
    if (DebuggedThread* thread = GetDebuggedThread(id)) {
      reply->threads.push_back(
          thread->GetThreadRecord(debug_ipc::ThreadRecord::StackAmount::kMinimal));
    }
  }
}

void DebugAgent::OnResume(const debug_ipc::ResumeRequest& request, debug_ipc::ResumeReply* reply) {
  DEBUG_LOG(Agent) << LogResumeRequest(request);

  if (request.ids.empty()) {
    // All debugged processes.
    for (const auto& pair : procs_)
      pair.second->OnResume(request);
  } else {
    // Explicit list.
    for (const auto& id : request.ids) {
      if (DebuggedProcess* proc = GetDebuggedProcess(id.process)) {
        if (id.thread) {
          // Single thread in that process.
          if (DebuggedThread* thread = proc->GetThread(id.thread)) {
            thread->ClientResume(request);
          } else {
            LOGS(Warn) << "Could not find thread by koid: " << id.thread;
          }
        } else {
          // All threads in the process.
          proc->OnResume(request);
        }
      } else {
        LOGS(Warn) << "Could not find process by koid: " << id.process;
      }
    }
  }
}

void DebugAgent::OnModules(const debug_ipc::ModulesRequest& request,
                           debug_ipc::ModulesReply* reply) {
  DebuggedProcess* proc = GetDebuggedProcess(request.process_koid);
  if (proc)
    proc->OnModules(reply);
}

void DebugAgent::OnProcessTree(const debug_ipc::ProcessTreeRequest& request,
                               debug_ipc::ProcessTreeReply* reply) {
  reply->root = system_interface_->GetProcessTree();
}

void DebugAgent::OnThreads(const debug_ipc::ThreadsRequest& request,
                           debug_ipc::ThreadsReply* reply) {
  auto found = procs_.find(request.process_koid);
  if (found == procs_.end())
    return;

  reply->threads = found->second->GetThreadRecords();
}

void DebugAgent::OnReadMemory(const debug_ipc::ReadMemoryRequest& request,
                              debug_ipc::ReadMemoryReply* reply) {
  DebuggedProcess* proc = GetDebuggedProcess(request.process_koid);
  if (proc)
    proc->OnReadMemory(request, reply);
}

void DebugAgent::OnReadRegisters(const debug_ipc::ReadRegistersRequest& request,
                                 debug_ipc::ReadRegistersReply* reply) {
  DebuggedThread* thread = GetDebuggedThread(request.id);
  if (thread) {
    reply->registers = thread->ReadRegisters(request.categories);
  } else {
    LOGS(Error) << "Cannot find thread with koid: " << request.id.thread;
  }
}

void DebugAgent::OnWriteRegisters(const debug_ipc::WriteRegistersRequest& request,
                                  debug_ipc::WriteRegistersReply* reply) {
  DebuggedThread* thread = GetDebuggedThread(request.id);
  if (thread) {
    reply->status = debug::Status();
    reply->registers = thread->WriteRegisters(request.registers);
  } else {
    reply->status = debug::Status("Can not find thread " + std::to_string(request.id.thread) +
                                  " to write registers.");
    LOGS(Error) << "Cannot find thread with koid: " << request.id.thread;
  }
}

void DebugAgent::OnAddOrChangeBreakpoint(const debug_ipc::AddOrChangeBreakpointRequest& request,
                                         debug_ipc::AddOrChangeBreakpointReply* reply) {
  switch (request.breakpoint.type) {
    case debug_ipc::BreakpointType::kSoftware:
    case debug_ipc::BreakpointType::kHardware:
    case debug_ipc::BreakpointType::kReadWrite:
    case debug_ipc::BreakpointType::kWrite:
      return SetupBreakpoint(request, reply);
    case debug_ipc::BreakpointType::kLast:
      break;
  }

  FX_NOTREACHED() << "Invalid Breakpoint Type: " << static_cast<int>(request.breakpoint.type);
}

void DebugAgent::OnRemoveBreakpoint(const debug_ipc::RemoveBreakpointRequest& request,
                                    debug_ipc::RemoveBreakpointReply* reply) {
  RemoveBreakpoint(request.breakpoint_id);
}

void DebugAgent::OnSysInfo(const debug_ipc::SysInfoRequest& request,
                           debug_ipc::SysInfoReply* reply) {
  reply->version = system_interface_->GetSystemVersion();
  reply->num_cpus = system_interface_->GetNumCpus();
  reply->memory_mb = system_interface_->GetPhysicalMemory() / kMegabyte;

  reply->hw_breakpoint_count = arch::GetHardwareBreakpointCount();
  reply->hw_watchpoint_count = arch::GetHardwareWatchpointCount();
}

void DebugAgent::OnThreadStatus(const debug_ipc::ThreadStatusRequest& request,
                                debug_ipc::ThreadStatusReply* reply) {
  if (DebuggedThread* thread = GetDebuggedThread(request.id)) {
    reply->record = thread->GetThreadRecord(debug_ipc::ThreadRecord::StackAmount::kFull);
  } else {
    // When the thread is not found the thread record is set to "dead".
    reply->record.id = request.id;
    reply->record.state = debug_ipc::ThreadRecord::State::kDead;
  }
}

debug::Status DebugAgent::RegisterBreakpoint(Breakpoint* bp, zx_koid_t process_koid,
                                             uint64_t address) {
  DebuggedProcess* proc = GetDebuggedProcess(process_koid);
  if (proc)
    return proc->RegisterBreakpoint(bp, address);

  // The process might legitimately be not found if there was a race between
  // the process terminating and a breakpoint add/change.
  return debug::Status("Process not found when adding breakpoint");
}

void DebugAgent::UnregisterBreakpoint(Breakpoint* bp, zx_koid_t process_koid, uint64_t address) {
  // The process might legitimately be not found if it was terminated.
  DebuggedProcess* proc = GetDebuggedProcess(process_koid);
  if (proc)
    proc->UnregisterBreakpoint(bp, address);
}

void DebugAgent::SetupBreakpoint(const debug_ipc::AddOrChangeBreakpointRequest& request,
                                 debug_ipc::AddOrChangeBreakpointReply* reply) {
  uint32_t id = request.breakpoint.id;
  auto found = breakpoints_.find(id);
  if (found == breakpoints_.end()) {
    DEBUG_LOG(Agent) << "Creating new breakpoint " << request.breakpoint.id << " ("
                     << request.breakpoint.name << ").";
    found = breakpoints_
                .emplace(std::piecewise_construct, std::forward_as_tuple(id),
                         std::forward_as_tuple(this))
                .first;
  }

  reply->status = found->second.SetSettings(request.breakpoint);
}

debug::Status DebugAgent::RegisterWatchpoint(Breakpoint* bp, zx_koid_t process_koid,
                                             const debug::AddressRange& range) {
  DebuggedProcess* proc = GetDebuggedProcess(process_koid);
  if (proc)
    return proc->RegisterWatchpoint(bp, range);

  // The process might legitimately be not found if there was a race between the process terminating
  // and a breakpoint add/change.
  return debug::Status("Process not found when adding watchpoint");
}

void DebugAgent::UnregisterWatchpoint(Breakpoint* bp, zx_koid_t process_koid,
                                      const debug::AddressRange& range) {
  // The process might legitimately be not found if it was terminated.
  DebuggedProcess* proc = GetDebuggedProcess(process_koid);
  if (proc)
    proc->UnregisterWatchpoint(bp, range);
}

void DebugAgent::OnAddressSpace(const debug_ipc::AddressSpaceRequest& request,
                                debug_ipc::AddressSpaceReply* reply) {
  DebuggedProcess* proc = GetDebuggedProcess(request.process_koid);
  if (proc)
    proc->OnAddressSpace(request, reply);
}

void DebugAgent::OnUpdateFilter(const debug_ipc::UpdateFilterRequest& request,
                                debug_ipc::UpdateFilterReply* reply) {
  DEBUG_LOG(Agent) << "Received UpdateFilter request size=" << request.filters.size();
  filters_.clear();
  filters_.reserve(request.filters.size());
  for (const auto& filter : request.filters) {
    filters_.emplace_back(filter);
    auto matched_processes =
        filters_.back().ApplyToJob(root_job_->job_handle(), *system_interface_);
    if (!matched_processes.empty()) {
      reply->matched_processes_for_filter.emplace_back(filter.id, std::move(matched_processes));
    }
  }
}

void DebugAgent::OnWriteMemory(const debug_ipc::WriteMemoryRequest& request,
                               debug_ipc::WriteMemoryReply* reply) {
  DebuggedProcess* proc = GetDebuggedProcess(request.process_koid);
  if (proc) {
    proc->OnWriteMemory(request, reply);
  } else {
    reply->status = debug::Status("Not attached to process " +
                                  std::to_string(request.process_koid) + " while writing memory.");
  }
}

void DebugAgent::OnLoadInfoHandleTable(const debug_ipc::LoadInfoHandleTableRequest& request,
                                       debug_ipc::LoadInfoHandleTableReply* reply) {
  DebuggedProcess* proc = GetDebuggedProcess(request.process_koid);
  if (proc)
    proc->OnLoadInfoHandleTable(request, reply);
  else
    reply->status =
        debug::Status("Not attached to process " + std::to_string(request.process_koid) +
                      " while getting the handle table.");
}

void DebugAgent::OnUpdateGlobalSettings(const debug_ipc::UpdateGlobalSettingsRequest& request,
                                        debug_ipc::UpdateGlobalSettingsReply* reply) {
  for (const auto& update : request.exception_strategies) {
    exception_strategies_[update.type] = update.value;
  }
}

void DebugAgent::OnSaveMinidump(const debug_ipc::SaveMinidumpRequest& request,
                                debug_ipc::SaveMinidumpReply* reply) {
  reply->status = debug::Status();

  DebuggedProcess* proc = GetDebuggedProcess(request.process_koid);

  if (!proc) {
    reply->status =
        debug::Status("No process found to save core from. Is there an attached process?");
    return;
  }

  proc->OnSaveMinidump(request, reply);
}

DebuggedJob* DebugAgent::GetDebuggedJob(zx_koid_t koid) {
  auto found = jobs_.find(koid);
  if (found == jobs_.end()) {
    return nullptr;
  }

  return found->second.get();
}

DebuggedProcess* DebugAgent::GetDebuggedProcess(zx_koid_t koid) {
  auto found = procs_.find(koid);
  if (found == procs_.end())
    return nullptr;
  return found->second.get();
}

DebuggedThread* DebugAgent::GetDebuggedThread(const debug_ipc::ProcessThreadId& id) {
  DebuggedProcess* process = GetDebuggedProcess(id.process);
  if (!process)
    return nullptr;
  return process->GetThread(id.thread);
}

std::vector<debug_ipc::ProcessThreadId> DebugAgent::ClientSuspendAll(zx_koid_t except_process,
                                                                     zx_koid_t except_thread) {
  // Neither or both koids must be supplied.
  FX_DCHECK((except_process == ZX_KOID_INVALID && except_thread == ZX_KOID_INVALID) ||
            (except_process != ZX_KOID_INVALID && except_thread != ZX_KOID_INVALID));

  std::vector<debug_ipc::ProcessThreadId> affected;

  for (const auto& [process_koid, process] : procs_) {
    std::vector<debug_ipc::ProcessThreadId> proc_threads;
    if (process_koid == except_process) {
      proc_threads = process->ClientSuspendAllThreads(except_thread);
    } else {
      proc_threads = process->ClientSuspendAllThreads();
    }

    affected.insert(affected.end(), proc_threads.begin(), proc_threads.end());
  }

  return affected;
}

bool DebugAgent::IsAttachedToParentOrAncestorOf(zx_koid_t parent) {
  while (parent != ZX_KOID_INVALID && parent != root_job_->koid()) {
    auto debugged_job = GetDebuggedJob(parent);
    if (debugged_job && debugged_job->type() == JobExceptionChannelType::kException) {
      // We are already attached to a parent job between the process and the root job.
      return true;
    }

    // Note if the process was torn down asynchronously by someone else, the ancestry tree could be
    // gone already, resulting in this being ZX_KOID_INVALID.
    //
    // TODO(https://fxbug.dev/377671670): Write better tests for this.
    parent = system_interface().GetParentJobKoid(parent);
  }

  return false;
}

bool DebugAgent::IsAttachedToParentOrAncestorOf(const ProcessHandle* process) {
  return IsAttachedToParentOrAncestorOf(process->GetJobKoid());
}

debug::Status DebugAgent::AddDebuggedJob(DebuggedJobCreateInfo&& create_info, DebuggedJob** added) {
  *added = nullptr;

  // This function should never be called with a null handle. At worst, something in the system has
  // deleted this job out from under us, causing all syscalls using this handle to fail, but the
  // object should still be valid.
  FX_DCHECK(create_info.handle);

  zx_koid_t job_koid = create_info.handle->GetKoid();
  auto unique = std::make_unique<DebuggedJob>(this);
  *added = unique.get();

  jobs_[job_koid] = std::move(unique);

  if (auto status = (*added)->Init(std::move(create_info)); status.has_error()) {
    jobs_.erase(job_koid);
    *added = nullptr;
    return status;
  }

  return debug::Status();
}

debug::Status DebugAgent::AddDebuggedProcess(DebuggedProcessCreateInfo&& create_info,
                                             DebuggedProcess** new_process) {
  *new_process = nullptr;

  auto proc = std::make_unique<DebuggedProcess>(this);

  // Need to register the process before calling DebuggedProcess::Init() because Init() can
  // do things like make breakpoints that call back into this class.
  auto process_id = create_info.handle->GetKoid();
  *new_process = proc.get();
  procs_[process_id] = std::move(proc);

  if (auto status = (*new_process)->Init(std::move(create_info)); status.has_error()) {
    // Undo registration.
    procs_.erase(process_id);
    *new_process = nullptr;
    return status;
  }
  return debug::Status();
}

debug_ipc::ExceptionStrategy DebugAgent::GetExceptionStrategy(debug_ipc::ExceptionType type) {
  auto strategy = exception_strategies_.find(type);
  if (strategy == exception_strategies_.end()) {
    return debug_ipc::ExceptionStrategy::kFirstChance;
  }
  return strategy->second;
}

// Attaching ---------------------------------------------------------------------------------------

void DebugAgent::OnAttach(const debug_ipc::AttachRequest& request, debug_ipc::AttachReply* reply) {
  DEBUG_LOG(Agent) << "Attemping to attach to process " << request.koid;
  reply->timestamp = GetNowTimestamp();

  // See if we already have a DebuggedProcess for this koid. If we do and we're not already attached
  // (we could be attached to the parent job already), try to attach. This might fail if another
  // process eagerly claimed the exception channel before us.
  if (request.config.target == debug_ipc::AttachConfig::Target::kProcess) {
    if (auto found = procs_.find(request.koid); found != procs_.end()) {
      DebuggedProcess* proc = found->second.get();
      if (proc->IsAttached()) {
        reply->status =
            debug::Status(debug::Status::kAlreadyExists,
                          "Already attached to process " + std::to_string(proc->koid()));
        DEBUG_LOG(Agent) << reply->status.message();
        return;
      }

      // If we get here it means we deferred binding the exception channel of this process, and the
      // client has requested explicitly to attach.
      reply->status = proc->AttachNow();
      if (reply->status.has_error()) {
        DEBUG_LOG(Agent) << std::format("Could not attach to process {}", proc->koid())
                         << reply->status.message();
      }

      DEBUG_LOG(Agent) << std::format("Attached to process {}", proc->koid());
      return;
    }
  }

  // First check if the process is in limbo. Sends the appropiate replies/notifications.
  if (system_interface_->GetLimboProvider().Valid()) {
    reply->status = AttachToLimboProcess(request.koid, reply);
    if (reply->status.ok())
      return;

    DEBUG_LOG(Agent) << "Could not attach to process in limbo: " << reply->status.message();
  }

  if (request.config.target == debug_ipc::AttachConfig::Target::kJob) {
    if (!IsAttachedToParentOrAncestorOf(request.koid)) {
      reply->status = AttachToExistingJob(request.koid, request.config, reply);
    } else {
      reply->status =
          debug::Status(debug::Status::kAlreadyExists, "Already attached to ancestor job.");
    }

    return;
  }

  // Attempt to attach to an existing process. Sends the appropriate replies/notifications.
  reply->status = AttachToExistingProcess(request.koid, request.config, reply);
  if (reply->status.ok())
    return;

  // We didn't find a process.
  DEBUG_LOG(Agent) << "Could not attach to existing process: " << reply->status.message();
}

debug::Status DebugAgent::AttachToLimboProcess(zx_koid_t process_koid,
                                               debug_ipc::AttachReply* reply) {
  LimboProvider& limbo = system_interface_->GetLimboProvider();
  FX_DCHECK(limbo.Valid());

  // Obtain the process and exception from limbo.
  auto retrieved = limbo.RetrieveException(process_koid);
  if (retrieved.is_error()) {
    debug::Status status = retrieved.error_value();
    DEBUG_LOG(Agent) << "Could not retrieve exception from limbo: " << status.message();
    return status;
  }

  LimboProvider::RetrievedException& exception = retrieved.value();

  DebuggedProcessCreateInfo create_info(std::move(exception.process));
  create_info.from_limbo = true;

  DebuggedProcess* process = nullptr;
  debug::Status status = AddDebuggedProcess(std::move(create_info), &process);
  if (status.has_error())
    return status;

  reply->koid = process->koid();
  reply->name = process->process_handle().GetName();
  reply->components =
      system_interface_->GetComponentManager().FindComponentInfo(process->process_handle());

  // Send the reply first, then the notifications about the process and threads.
  debug::MessageLoop::Current()->PostTask(FROM_HERE, [weak_this = GetWeakPtr(), koid = reply->koid,
                                                      exception = std::move(exception)]() mutable {
    if (!weak_this)
      return;
    if (DebuggedProcess* process = weak_this->GetDebuggedProcess(koid)) {
      process->PopulateCurrentThreads();
      process->SuspendAndSendModules();

      zx_koid_t thread_koid = exception.thread->GetKoid();

      // Pass in the exception handle to the corresponding thread.
      DebuggedThread* exception_thread = nullptr;
      for (DebuggedThread* thread : process->GetThreads()) {
        if (thread->koid() == thread_koid) {
          exception_thread = thread;
          break;
        }
      }

      if (exception_thread)
        exception_thread->set_exception_handle(std::move(exception.exception));
    }
  });

  return debug::Status();
}

debug::Status DebugAgent::AttachToExistingProcess(zx_koid_t process_koid,
                                                  const debug_ipc::AttachConfig& config,
                                                  debug_ipc::AttachReply* reply) {
  std::unique_ptr<ProcessHandle> process_handle = system_interface_->GetProcess(process_koid);
  if (!process_handle)
    return debug::Status("Can't find process " + std::to_string(process_koid) + " to attach to.");

  DebuggedProcess* process = nullptr;
  DebuggedProcessCreateInfo create_info(std::move(process_handle));
  create_info.weak = config.weak;
  create_info.deferred_attach = config.target == debug_ipc::AttachConfig::Target::kJob;
  if (auto status = AddDebuggedProcess(std::move(create_info), &process); status.has_error())
    return status;

  reply->koid = process->koid();
  reply->name = process->process_handle().GetName();
  reply->components =
      system_interface_->GetComponentManager().FindComponentInfo(process->process_handle());

  // Send the reply first, then the notifications about the process and threads.
  debug::MessageLoop::Current()->PostTask(
      FROM_HERE, [weak_this = GetWeakPtr(), koid = reply->koid, config]() mutable {
        if (!weak_this)
          return;
        if (DebuggedProcess* process = weak_this->GetDebuggedProcess(koid)) {
          process->PopulateCurrentThreads();
          if (!ShouldDeferSendingModules(config))
            process->SuspendAndSendModules();
        }
      });

  return debug::Status();
}

debug::Status DebugAgent::AttachToRootJob() {
  DebuggedJobCreateInfo info(system_interface().GetRootJob());
  // Only ever attach to the root job's debugger channel.
  info.type = JobExceptionChannelType::kDebugger;

  // The root job is otherwise treated just like any other job.
  auto status = AddDebuggedJob(std::move(info), &root_job_);

  // This function should only be called on Fuchsia, and we should always be able to get the
  // root job.
  FX_DCHECK(status.ok());
  FX_DCHECK(root_job_ != nullptr);

  return status;
}

debug::Status DebugAgent::AttachToExistingJob(zx_koid_t job_koid,
                                              const debug_ipc::AttachConfig& config,
                                              debug_ipc::AttachReply* reply) {
  DebuggedJob* debugged_job;
  DebuggedJobCreateInfo info(system_interface().GetJob(job_koid));
  info.type =
      config.weak ? JobExceptionChannelType::kDebugger : JobExceptionChannelType::kException;

  // Check the validity of the JobHandle here. We don't pass the job's koid to |AddDebuggedJob|, so
  // it can only get the koid by dereferencing the JobHandle, so we can print a better error message
  // here.
  if (info.handle == nullptr) {
    return debug::Status(std::format("Job with koid {} not found", job_koid));
  }

  auto status = AddDebuggedJob(std::move(info), &debugged_job);

  if (status.has_error()) {
    return status;
  }

  // Won't have a reply object to fill out if this comes from a filter.
  if (reply) {
    reply->components = system_interface().GetComponentManager().FindComponentInfo(job_koid);
    reply->koid = job_koid;
  }

  return status;
}

void DebugAgent::LaunchProcess(const debug_ipc::RunBinaryRequest& request,
                               debug_ipc::RunBinaryReply* reply) {
  FX_DCHECK(!request.argv.empty());
  DEBUG_LOG(Process) << "Launching binary " << request.argv.front();

  std::unique_ptr<BinaryLauncher> launcher = system_interface_->GetLauncher();
  reply->status = launcher->Setup(request.argv);
  if (reply->status.has_error())
    return;

  DebuggedProcessCreateInfo create_info(launcher->GetProcess());
  create_info.stdio = launcher->ReleaseStdioHandles();

  // The DebuggedProcess must be attached to the new process' exception port before actually
  // Starting the process to avoid racing with the program initialization.
  DebuggedProcess* new_process = nullptr;
  reply->status = AddDebuggedProcess(std::move(create_info), &new_process);
  if (reply->status.has_error())
    return;

  reply->status = launcher->Start();
  if (reply->status.has_error()) {
    RemoveDebuggedProcess(new_process->koid());
    return;
  }

  // Success, fill out the reply.
  reply->process_id = new_process->koid();
  reply->process_name = new_process->process_handle().GetName();
}

void DebugAgent::OnProcessChanged(ProcessChangedHow how,
                                  std::unique_ptr<ProcessHandle> process_handle) {
  if (procs_.find(process_handle->GetKoid()) != procs_.end()) {
    return;  // The process might have been attached in |LaunchProcess|.
  }

  debug_ipc::NotifyProcessStarting::Type type = debug_ipc::NotifyProcessStarting::Type::kLast;
  StdioHandles stdio;  // Will be filled in only for components.
  std::string process_name_override;
  const debug_ipc::Filter* matched_filter = nullptr;

  if (how == ProcessChangedHow::kStarting &&
      system_interface_->GetComponentManager().OnProcessStart(*process_handle, &stdio,
                                                              &process_name_override)) {
    type = debug_ipc::NotifyProcessStarting::Type::kLaunch;
  } else if (std::any_of(filters_.begin(), filters_.end(), [&](const Filter& filter) {
               if (filter.MatchesProcess(*process_handle, *system_interface_)) {
                 matched_filter = &filter.filter();
                 return true;
               }
               return false;
             })) {
    type = debug_ipc::NotifyProcessStarting::Type::kAttach;
  } else {
#ifdef __linux__
    // For now, unconditionally attach to all forked processes on Linux.
    // TODO(brettw) This should be revisited when we get better frontend UI for dealing with forks.
    type = debug_ipc::NotifyProcessStarting::Type::kAttach;
#else
    return;
#endif
  }

  bool weak = matched_filter ? matched_filter->config.weak : false;
  bool job_only = matched_filter ? matched_filter->config.job_only : false;

  // If we have a job only filter then we only watch for exceptions from the parent job and do not
  // attach to the process (but we do create a DebuggedProcess object for it below).
  if (job_only) {
    // Already attached, nothing to do. This path is quite common when we are attaching to jobs
    // located relatively high in a job tree which can spawn many processes. We will get many
    // notifications of new processes.
    if (GetDebuggedJob(process_handle->GetJobKoid())) {
      return;
    }

    // There's nothing to stop a user from installing a filter that matches a child component with
    // its own unique job_id and then another filter that matches a parent, so this won't completely
    // stop you from attaching to multiple jobs in the job tree. If this happens, releasing an
    // exception reported from the child job job will result in us catching it again at the next job
    // in the tree we're attached to, which could be confusing.
    if (!IsAttachedToParentOrAncestorOf(process_handle.get())) {
      debug_ipc::AttachConfig config;
      config.weak = matched_filter->config.weak;

      auto status = AttachToExistingJob(process_handle->GetJobKoid(), config, nullptr);
      if (status.has_error()) {
        LOGS(Warn) << "AttachToJob failed for job " << process_handle->GetJobKoid() << ": "
                   << status.message();
      }
    }

    // We can't return yet because we need to tell the front end about this process and its threads
    // because the front end needs to be able to associate all exceptions with a process and thread.
    // Importantly, the DebuggedProcessCreateInfo will defer claiming the process's exception
    // channel.
  }

  DEBUG_LOG(Process) << "Process starting, koid: " << process_handle->GetKoid();

  // Prepare the notification but don't send yet because |process_handle| will be moved and
  // |AddDebuggedProcess| may fail.
  debug_ipc::NotifyProcessStarting notify;
  notify.type = type;
  notify.koid = process_handle->GetKoid();
  notify.name = process_name_override.empty() ? process_handle->GetName() : process_name_override;
  notify.timestamp = GetNowTimestamp();
  notify.components = system_interface_->GetComponentManager().FindComponentInfo(*process_handle);
  notify.filter_id = matched_filter ? matched_filter->id : debug_ipc::kInvalidFilterId;

  DebuggedProcessCreateInfo create_info(std::move(process_handle));
  create_info.stdio = std::move(stdio);
  create_info.weak = weak;
  create_info.deferred_attach = job_only;

  DebuggedProcess* new_process = nullptr;
  debug::Status status = AddDebuggedProcess(std::move(create_info), &new_process);

  if (status.has_error()) {
    LOGS(Warn) << "Failed to attach to process " << notify.koid << ": " << status.message();
    return;
  }

  SendNotification(notify);

  new_process->PopulateCurrentThreads();

  // If this is a strong attach, we need to send modules here. We cannot wait for the client to
  // request all the modules later because we won't be able to load symbols early enough to set
  // breakpoints on things like _dl_start, which will resolve from the first modules being sent
  // now. The rest of the modules will be sent later on when the client requests them or we hit the
  // loader breakpoint.
  if (!debug_ipc::FilterDefersModules(matched_filter)) {
    new_process->SuspendAndSendModules();
  }
}

void DebugAgent::OnComponentDiscovered(const std::string& moniker, const std::string& url) {
  auto matched_filters = GetMatchingFiltersForComponentInfo(moniker, url);

  for (auto filter : matched_filters) {
    if (filter != nullptr && filter->filter().config.recursive) {
      // When any recursive filter matches here, we install a component moniker prefix filter so
      // that any sub-components created as children of this one are attached implicitly. Only one
      // filter match needs to be recursive for us to install the prefix filter for |moniker|, and
      // we only need to install one new filter per invocation of this function. The client is
      // notified of this filter so that it is not removed on subsequent UpdateFilter requests,
      // which the client will do shortly after receiving this notification. The new version of
      // this filter will include a filter id and with all of the settings given here. Notably, we
      // do not enable the recursive flag on this filter, which would be redundant with the parent
      // filter.
      debug_ipc::Filter realm_filter;
      realm_filter.type = debug_ipc::Filter::Type::kComponentMonikerPrefix;
      realm_filter.pattern = moniker;
      realm_filter.config.weak = filter->filter().config.weak;

      filters_.emplace_back(realm_filter);

      debug_ipc::NotifyComponentDiscovered notify;
      notify.filter = realm_filter;

      SendNotification(notify);
      return;
    }
  }
}

void DebugAgent::OnComponentStarted(const std::string& moniker, const std::string& url) {
  if (!GetMatchingFiltersForComponentInfo(moniker, url).empty()) {
    debug_ipc::NotifyComponentStarting notify;
    notify.component.moniker = moniker;
    notify.component.url = url;
    notify.timestamp = GetNowTimestamp();

    SendNotification(notify);
  }
}

void DebugAgent::OnComponentExited(const std::string& moniker, const std::string& url) {
  if (!GetMatchingFiltersForComponentInfo(moniker, url).empty()) {
    debug_ipc::NotifyComponentExiting notify;
    notify.component.moniker = moniker;
    notify.component.url = url;
    notify.timestamp = GetNowTimestamp();

    SendNotification(notify);
  }
}

void DebugAgent::OnTestComponentExited(const std::string& url) {
  debug_ipc::NotifyTestExited notify;
  notify.url = url;
  notify.timestamp = GetNowTimestamp();

  SendNotification(notify);
}

void DebugAgent::InjectProcessForTest(std::unique_ptr<DebuggedProcess> process) {
  procs_[process->koid()] = std::move(process);
}

void DebugAgent::OnProcessEnteredLimbo(const LimboProvider::Record& record) {
  zx_koid_t process_koid = record.process->GetKoid();

  // First check if we were to "kill" this process.
  if (auto it = killed_limbo_procs_.find(process_koid); it != killed_limbo_procs_.end()) {
    system_interface_->GetLimboProvider().ReleaseProcess(process_koid);
    killed_limbo_procs_.erase(it);
    return;
  }

  std::string process_name = record.process->GetName();
  DEBUG_LOG(Agent) << "Process " << process_name << " (" << process_koid << ") entered limbo.";

  debug_ipc::NotifyProcessStarting process_starting = {};
  process_starting.type = debug_ipc::NotifyProcessStarting::Type::kLimbo;
  process_starting.koid = process_koid;
  process_starting.name = std::move(process_name);
  process_starting.timestamp = GetNowTimestamp();

  SendNotification(process_starting);
}

std::vector<const Filter*> DebugAgent::GetMatchingFiltersForComponentInfo(
    const std::string& moniker, const std::string& url) const {
  std::vector<const Filter*> matches;
  for (const auto& filter : filters_) {
    if (filter.MatchesComponent(moniker, url)) {
      matches.push_back(&filter);
    }
  }

  return matches;
}

void DebugAgent::RemoveObserver(DebugAgentObserver* observer) {
  observers_.RemoveObserver(observer);

  // If we just removed the last server and there is no debug_ipc client, it's time to exit.
  if (!observers_.might_have_observers() && !is_connected()) {
    ClearState();
    debug::MessageLoop::Current()->QuitNow();
  }
}

void DebugAgent::WriteLog(debug::LogSeverity severity, const debug::FileLineFunction& location,
                          std::string log) {
  debug_ipc::NotifyLog notify;
  switch (severity) {
    case debug::LogSeverity::kInfo:
      return;  // Only forward warnings and errors for now.
    case debug::LogSeverity::kWarn:
      notify.severity = debug_ipc::NotifyLog::Severity::kWarn;
      break;
    case debug::LogSeverity::kError:
      notify.severity = debug_ipc::NotifyLog::Severity::kError;
      break;
  }
  notify.location.file = location.file();
  notify.location.function = location.function();
  notify.location.line = location.line();
  notify.log = log;

  SendNotification(notify);
}

}  // namespace debug_agent
