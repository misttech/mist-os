// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/zircon_job_handle.h"

#include <lib/syslog/cpp/macros.h>

#include "src/developer/debug/debug_agent/zircon_exception_handle.h"
#include "src/developer/debug/debug_agent/zircon_process_handle.h"
#include "src/developer/debug/debug_agent/zircon_utils.h"
#include "src/developer/debug/shared/logging/logging.h"
#include "src/developer/debug/shared/message_loop_fuchsia.h"

namespace debug_agent {

ZirconJobHandle::ZirconJobHandle(zx::job j)
    : job_koid_(zircon::KoidForObject(j)), job_(std::move(j)) {}

ZirconJobHandle::ZirconJobHandle(const ZirconJobHandle& other) : job_koid_(other.job_koid_) {
  other.job_.duplicate(ZX_RIGHT_SAME_RIGHTS, &job_);
}

std::unique_ptr<JobHandle> ZirconJobHandle::Duplicate() const {
  return std::make_unique<ZirconJobHandle>(*this);
}

std::string ZirconJobHandle::GetName() const { return zircon::NameForObject(job_); }

std::vector<std::unique_ptr<JobHandle>> ZirconJobHandle::GetChildJobs() const {
  std::vector<std::unique_ptr<JobHandle>> result;
  for (auto& zx_job : zircon::GetChildJobs(job_))
    result.push_back(std::make_unique<ZirconJobHandle>(std::move(zx_job)));
  return result;
}

std::vector<std::unique_ptr<ProcessHandle>> ZirconJobHandle::GetChildProcesses() const {
  std::vector<std::unique_ptr<ProcessHandle>> result;
  for (auto& zx_process : zircon::GetChildProcesses(job_))
    result.push_back(std::make_unique<ZirconProcessHandle>(std::move(zx_process)));
  return result;
}

debug::Status ZirconJobHandle::WatchJobExceptions(JobExceptionObserver* observer,
                                                  JobExceptionChannelType type) {
  debug::Status status;

  if (!observer) {
    // Unregistering.
    job_watch_handle_.StopWatching();
  } else if (!exception_observer_) {
    // Registering for the first time.
    debug::MessageLoopFuchsia* loop = debug::MessageLoopFuchsia::Current();
    FX_DCHECK(loop);  // Loop must be created on this thread first.

    DEBUG_LOG(Agent) << "Registering for JobExceptions";
    debug::MessageLoopFuchsia::WatchJobConfig config;
    config.job_name = GetName();
    config.job_handle = job_.get();
    config.job_koid = job_koid_;
    config.use_debugger_channel = type == JobExceptionChannelType::kDebugger;
    config.watcher = this;
    status = debug::ZxStatus(loop->WatchJobExceptions(std::move(config), &job_watch_handle_));
  }

  exception_observer_ = observer;
  return status;
}

void ZirconJobHandle::OnJobException(zx::exception exception, zx_exception_info_t exception_info) {
  zx::process process;
  zx_status_t status = exception.get_process(&process);
  if (status != ZX_OK) {
    LOGS(Warn) << "Failed to get process " << exception_info.pid
               << " from exception_info: " << zx_status_get_string(status);
    return;
  }
  auto process_handle = std::make_unique<ZirconProcessHandle>(std::move(process));

  zx::thread thread;
  status = exception.get_thread(&thread);
  if (status != ZX_OK) {
    LOGS(Warn) << "Failed to get thread " << exception_info.tid
               << " from exception_info: " << zx_status_get_string(status);
    return;
  }

  zx_exception_report_t report = {};
  status =
      thread.get_info(ZX_INFO_THREAD_EXCEPTION_REPORT, &report, sizeof(report), nullptr, nullptr);
  if (status != ZX_OK) {
    LOGS(Warn) << "Failed to get thread exception report for thread " << exception_info.tid << ": "
               << zx_status_get_string(status);
    return;
  }

  if (exception_info.type == ZX_EXCP_PROCESS_STARTING) {
    exception_observer_->OnProcessStarting(std::move(process_handle));
  } else if (exception_info.type == ZX_EXCP_USER) {
    if (report.context.synth_code == ZX_EXCP_USER_CODE_PROCESS_NAME_CHANGED) {
      exception_observer_->OnProcessNameChanged(std::move(process_handle));
    }
  } else {
    // We got an exception that is traveling up the job tree, we intercept it here and report it to
    // observers so we can report it to clients that explicitly attached to the job instead of the
    // process itself. Note we will only receive these exceptions here if we are attached to the
    // job's non-debugger exception channel.
    exception_observer_->OnUnhandledException(
        std::make_unique<ZirconExceptionHandle>(std::move(exception), exception_info, report));
    return;
  }

  // Attached to the process. At that point it will get a new thread notification for the initial
  // thread which it can stop or continue as it desires. Therefore, we can always resume the thread
  // in the "new process" exception.
  //
  // Technically it's not necessary to reset the handle, but being explicit here helps readability.
  exception.reset();
}

}  // namespace debug_agent
