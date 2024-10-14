// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debugged_job.h"

#include "lib/syslog/cpp/macros.h"
#include "src/developer/debug/debug_agent/debug_agent.h"

namespace debug_agent {

DebuggedJobCreateInfo::DebuggedJobCreateInfo(std::unique_ptr<JobHandle> handle)
    : handle(std::move(handle)) {}

DebuggedJob::DebuggedJob(DebugAgent* debug_agent) : debug_agent_(debug_agent) {}

debug::Status DebuggedJob::Init(DebuggedJobCreateInfo&& info) {
  if (info.handle == nullptr) {
    return debug::Status("Cannot initialize DebuggedJob with an invalid JobHandle.");
  }

  job_handle_ = std::move(info.handle);

  if (auto status = job_handle_->WatchJobExceptions(this, info.type); status.has_error()) {
    return status;
  }

  return debug::Status();
}

void DebuggedJob::OnProcessStarting(std::unique_ptr<ProcessHandle> process) {
  FX_DCHECK(debug_agent_);

  debug_agent_->OnProcessChanged(true, std::move(process));
}

void DebuggedJob::OnProcessNameChanged(std::unique_ptr<ProcessHandle> process_handle) {
  FX_DCHECK(debug_agent_);

  debug_agent_->OnProcessChanged(false, std::move(process_handle));
}

void DebuggedJob::OnUnhandledException(std::unique_ptr<ExceptionHandle> exception_handle) {
  FX_DCHECK(debug_agent_);
  // This notification means that we were attached to a job and none of the processes within this
  // job handled the exception. By virtue of being attached to the job only, we are not actually
  // interactively debugging the exception, we just collect some information and report it to all
  // clients.
  //
  // Note that we do actually have a DebuggedProcess and DebuggedThread by virtue of the
  // ProcessStarting and ThreadStarting notifications, so we already have everything we need here.
  auto process = debug_agent_->GetDebuggedProcess(exception_handle->GetProcessHandle()->GetKoid());
  FX_DCHECK(process);
  auto thread = debug_agent_->GetDebuggedThread(
      {.process = process->koid(), .thread = exception_handle->GetThreadHandle()->GetKoid()});
  FX_DCHECK(thread);

  debug_ipc::NotifyException notify;
  notify.thread = thread->GetThreadRecord(debug_ipc::ThreadRecord::StackAmount::kFull);
  notify.exception = exception_handle->GetRecord();
  notify.timestamp = GetNowTimestamp();
  notify.type = exception_handle->GetType(thread->thread_handle());
  // Make sure to tell the client that we aren't actually holding onto the exception after the
  // notification has been sent.
  notify.job_only = true;

  debug_agent_->SendNotification(notify);

  // Stop watching this particular process, but keep the job. The job is what the user is attached
  // to and could have other processes running.
  debug_agent_->RemoveDebuggedProcess(process->koid());
  exception_handle.reset();
}

}  // namespace debug_agent
