// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/process_explorer/process_explorer.h"

#include <fuchsia/kernel/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <task-utils/walker.h>

#include "src/developer/process_explorer/utils.h"
#include "src/lib/fsl/socket/strings.h"

namespace process_explorer {
namespace {

class ProcessWalker : public TaskEnumerator {
 public:
  ProcessWalker() = default;
  ~ProcessWalker() = default;

  zx_status_t WalkProcessTree(std::vector<Process>* processes) {
    FX_CHECK(processes_.empty());

    if (auto status = WalkRootJobTree(); status != ZX_OK) {
      FX_LOGS(ERROR) << "Unable to walk job tree: " << zx_status_get_string(status);
      return status;
    }

    *processes = std::move(processes_);
    processes_.clear();
    return ZX_OK;
  }

  zx_status_t OnProcess(int depth, zx_handle_t process_handle, zx_koid_t koid,
                        zx_koid_t parent_koid) override {
    zx::unowned_process process(process_handle);
    char name[ZX_MAX_NAME_LEN];

    if (auto status = process->get_property(ZX_PROP_NAME, &name, sizeof(name)); status != ZX_OK) {
      FX_LOGS(ERROR) << "Unable to get process name: " << zx_status_get_string(status);
      return status;
    }

    std::vector<zx_info_handle_extended_t> handles;
    if (auto status = GetHandles(std::move(process), &handles); status != ZX_OK) {
      FX_LOGS(ERROR) << "Unable to get associated handles for process: "
                     << zx_status_get_string(status);
      return status;
    }

    std::vector<KernelObject> process_objects;
    for (auto const& handle : handles) {
      process_objects.push_back(
          {handle.type, handle.koid, handle.related_koid, handle.peer_owner_koid});
    }

    std::string process_name(name);
    processes_.push_back({koid, process_name, process_objects});

    return ZX_OK;
  }

 protected:
  bool has_on_process() const override { return true; }

 private:
  std::vector<Process> processes_;
};

zx_status_t GetProcessesData(std::vector<Process>* processes_data) {
  ProcessWalker process_walker;
  if (auto status = process_walker.WalkProcessTree(processes_data); status != ZX_OK) {
    FX_LOGS(ERROR) << "Unable to get processes data: " << zx_status_get_string(status);
    return status;
  }

  // TODO(fxbug.dev/60170): Remove call to FillPeerOwnerKoid (and remove
  // FillPeerOwnerKoid itself) after peer owner koids become populated by
  // the kernel.
  FillPeerOwnerKoid(*processes_data);

  return ZX_OK;
}

}  // namespace

Explorer::Explorer(async_dispatcher_t* dispatcher, component::OutgoingDirectory& outgoing) {
  ZX_ASSERT(outgoing
                .AddUnmanagedProtocol<fuchsia_process_explorer::Query>(
                    query_bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure))
                .is_ok());
  ZX_ASSERT(outgoing
                .AddUnmanagedProtocol<fuchsia_process_explorer::ProcessExplorer>(
                    explorer_bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure))
                .is_ok());
}

Explorer::~Explorer() = default;

void Explorer::WriteJsonProcessesData(WriteJsonProcessesDataRequest& request,
                                      WriteJsonProcessesDataCompleter::Sync& completer) {
  std::vector<Process> processes_data;
  if (auto status = GetProcessesData(&processes_data); status != ZX_OK) {
    // Returning immediately. Nothing will have been written on the socket which will let the client
    // know an error has occurred.
    return;
  }

  const std::string json_string = WriteProcessesDataAsJson(std::move(processes_data));

  // TODO(fxbug.dev/108528): change to asynchronous
  fsl::BlockingCopyFromString(json_string, request.socket());
}

void Explorer::GetTaskInfo(GetTaskInfoRequest& request, GetTaskInfoCompleter::Sync& completer) {
  // TODO(surajmalhtora): Implement.
}

void Explorer::GetHandleInfo(GetHandleInfoRequest& request,
                             GetHandleInfoCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Explorer::GetVmaps(GetVmapsRequest& request, GetVmapsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Explorer::GetStackTrace(GetStackTraceRequest& request,
                             GetStackTraceCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

class KillWalker final : public TaskEnumerator {
 public:
  KillWalker() = default;
  ~KillWalker() = default;

  zx::result<zx_koid_t> KillTask(std::variant<zx_koid_t, std::string> task) {
    task_to_kill_ = task;
    if (auto status = WalkRootJobTree(); status != ZX_ERR_STOP) {
      if (status == ZX_OK) {
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      return zx::error(status);
    }
    return zx::ok(task_killed_.value());
  }

  zx_status_t OnProcess(int depth, zx_handle_t process_handle, zx_koid_t koid,
                        zx_koid_t parent_koid) override {
    zx::unowned_process process(process_handle);
    if (auto task_koid = std::get_if<zx_koid_t>(&task_to_kill_); task_koid) {
      if (*task_koid != koid) {
        return ZX_OK;
      }
    } else {
      auto& process_name = std::get<std::string>(task_to_kill_);
      char name[ZX_MAX_NAME_LEN];

      if (auto status = process->get_property(ZX_PROP_NAME, &name, sizeof(name)); status != ZX_OK) {
        FX_LOGS(ERROR) << "Unable to get process name: " << zx_status_get_string(status);
        return status;
      }
      if (process_name != name) {
        return ZX_OK;
      }
    }
    zx_status_t status = process->kill();
    if (status != ZX_OK) {
      return status;
    }
    task_killed_ = koid;
    return ZX_ERR_STOP;
  }

  zx_status_t OnJob(int depth, zx_handle_t job_handle, zx_koid_t koid,
                    zx_koid_t parent_koid) override {
    zx::unowned_job job(job_handle);
    if (auto* task_koid = std::get_if<zx_koid_t>(&task_to_kill_);
        !task_koid || *task_koid != koid) {
      return ZX_OK;
    }
    zx_status_t status = job->kill();
    if (status != ZX_OK) {
      return status;
    }
    task_killed_ = koid;
    return ZX_ERR_STOP;
  }

 protected:
  bool has_on_job() const override { return true; }
  bool has_on_process() const override { return true; }

 private:
  std::variant<zx_koid_t, std::string> task_to_kill_;
  std::optional<zx_koid_t> task_killed_;
};

void Explorer::KillTask(KillTaskRequest& request, KillTaskCompleter::Sync& completer) {
  KillWalker walker;
  std::variant<zx_koid_t, std::string> task;
  if (request.koid().has_value()) {
    task = request.koid().value();
  } else if (request.process_name().has_value()) {
    task = request.process_name().value();
  } else {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }
  completer.Reply(walker.KillTask(task));
}

}  // namespace process_explorer
