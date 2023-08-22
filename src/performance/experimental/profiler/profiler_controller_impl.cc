// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "profiler_controller_impl.h"

#include <elf.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <fidl/fuchsia.sys2/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fit/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/handle.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/thread.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/system/ulib/elf-search/include/elf-search.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <ios>
#include <memory>
#include <mutex>
#include <sstream>
#include <unordered_map>
#include <utility>
#include <vector>

#include <src/lib/fsl/socket/strings.h>
#include <src/lib/unwinder/module.h>

#include "sampler.h"
#include "symbolization_context.h"
#include "symbolizer_markup.h"
#include "targets.h"
#include "taskfinder.h"

zx::result<> PopulateTargets(profiler::TargetTree& tree, TaskFinder::FoundTasks&& tasks) {
  for (auto&& [koid, job] : tasks.jobs) {
    zx::result<profiler::JobTarget> job_target = profiler::MakeJobTarget(std::move(job));
    if (job_target.is_error()) {
      // A job might exit in the time between us walking the tree and attempting to find its
      // children. Skip it in this case.
      continue;
    }
    if (zx::result res = tree.AddJob(std::move(*job_target)); res.is_error()) {
      FX_PLOGS(ERROR, res.status_value()) << "Failed to add job target";
      return res;
    }
  }

  for (auto&& [koid, process] : tasks.processes) {
    zx_info_handle_basic_t handle_info;
    if (process.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info), nullptr,
                         nullptr) != ZX_OK) {
      // A process might exit in the time between us walking the tree and attempting to find its
      // children. Skip it in this case.
      continue;
    }
    zx::result<profiler::ProcessTarget> process_target =
        profiler::MakeProcessTarget(std::move(process));
    if (process_target.is_error()) {
      continue;
    }

    if (zx::result res = tree.AddProcess(std::move(*process_target)); res.is_error()) {
      FX_PLOGS(ERROR, res.status_value()) << "Failed to add process target";
      return res;
    }
  }
  for (auto&& [koid, thread] : tasks.threads) {
    zx_info_handle_basic_t handle_info;
    if (thread.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info), nullptr,
                        nullptr) != ZX_OK) {
      continue;
    }

    // If we have a thread, we need to know what its parent process is
    // and then get a handle to it. Unfortunately, the "easiest" way to
    // do this is to walk the job tree again.
    TaskFinder finder;
    finder.AddProcess(handle_info.related_koid);
    zx::result<TaskFinder::FoundTasks> found_tasks = finder.FindHandles();
    if (found_tasks.is_error()) {
      continue;
    }
    if (found_tasks->processes.size() != 1) {
      FX_LOGS(ERROR) << "Found the wrong number of processes for thread: " << thread.get();
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    auto [pid, process] = std::move(found_tasks->processes[0]);

    profiler::ProcessTarget process_target{std::move(process), pid,
                                           std::unordered_map<zx_koid_t, profiler::ThreadTarget>{}};
    elf_search::ForEachModule(process_target.handle,
                              [&process_target](const elf_search::ModuleInfo& info) {
                                process_target.unwinder_data->modules.emplace_back(
                                    info.vaddr, &process_target.unwinder_data->memory,
                                    unwinder::Module::AddressMode::kProcess);
                              });

    if (zx::result<> res = tree.AddProcess(std::move(process_target)); res.is_error()) {
      // If the process already exists, then we'll just append to the existing one below
      if (res.status_value() != ZX_ERR_ALREADY_EXISTS) {
        FX_PLOGS(ERROR, res.status_value()) << "Failed to add process target";
        return res;
      }
    }

    if (zx::result res = tree.AddThread(pid, profiler::ThreadTarget{std::move(thread), koid});
        res.is_error()) {
      FX_PLOGS(ERROR, res.status_value()) << "Failed to add thread target: " << koid;
      return res;
    }
  }
  return zx::ok();
}

zx::result<zx_koid_t> ReadElfJobId(const fidl::SyncClient<fuchsia_io::Directory>& directory) {
  zx::result<fidl::Endpoints<fuchsia_io::File>> endpoints =
      fidl::CreateEndpoints<fuchsia_io::File>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  fit::result<fidl::OneWayStatus> res = directory->Open(
      {{.flags = fuchsia_io::OpenFlags::kRightReadable,
        .mode = {},
        .path = "elf/job_id",
        .object = fidl::ServerEnd<fuchsia_io::Node>(endpoints->server.TakeChannel())}});
  if (res.is_error()) {
    return zx::error(ZX_ERR_IO);
  }

  fidl::SyncClient<fuchsia_io::File> job_id_file{std::move(endpoints->client)};
  fidl::Result<fuchsia_io::File::Read> read_res =
      job_id_file->Read({{.count = fuchsia_io::kMaxTransferSize}});
  if (read_res.is_error()) {
    return zx::error(ZX_ERR_IO);
  }
  std::string job_id_str(reinterpret_cast<const char*>(read_res->data().data()),
                         read_res->data().size());

  char* end;
  zx_koid_t job_id = std::strtoull(job_id_str.c_str(), &end, 10);
  if (end != job_id_str.c_str() + job_id_str.size()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  return zx::ok(job_id);
}

zx::result<zx_koid_t> MonikerToJobId(const std::string& moniker) {
  zx::result<fidl::ClientEnd<fuchsia_sys2::RealmQuery>> client_end =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (client_end.is_error()) {
    FX_LOGS(WARNING) << "Unable to connect to RealmQuery. Attaching by moniker isn't supported!";
    return client_end.take_error();
  }
  zx::result<fidl::Endpoints<fuchsia_io::Directory>> directory_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (directory_endpoints.is_error()) {
    FX_LOGS(WARNING) << "Unable to create directory endpoints";
    return directory_endpoints.take_error();
  }
  fidl::SyncClient<fuchsia_io::Directory> directory_client{std::move(directory_endpoints->client)};
  fidl::SyncClient realm_query_client{std::move(*client_end)};

  fidl::Result<fuchsia_sys2::RealmQuery::Open> open_result = realm_query_client->Open({{
      .moniker = moniker,
      .dir_type = fuchsia_sys2::OpenDirType::kRuntimeDir,
      .flags = fuchsia_io::OpenFlags::kRightReadable,
      .mode = {},
      .path = ".",
      .object = fidl::ServerEnd<fuchsia_io::Node>{directory_endpoints->server.TakeChannel()},
  }});
  if (open_result.is_error()) {
    FX_LOGS(WARNING) << "Unable to open the runtime directory of " << moniker << ": "
                     << open_result.error_value();
    return zx::error(ZX_ERR_BAD_PATH);
  }
  zx::result<zx_koid_t> job_id = ReadElfJobId(directory_client);
  if (job_id.is_error()) {
    FX_LOGS(WARNING) << "Unable to read component directory";
  }
  return job_id;
}

void profiler::ProfilerControllerImpl::Configure(ConfigureRequest& request,
                                                 ConfigureCompleter::Sync& completer) {
  if (state_ != ProfilingState::Unconfigured) {
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kBadState));
    return;
  }
  if (!request.output()) {
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kBadSocket));
    return;
  }
  socket_ = std::move(*request.output());

  if (!request.config() || !request.config()->target()) {
    FX_LOGS(ERROR) << "No Target Specified and System Wide profiling isn't yet implemented!";
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
    return;
  }

  // We're given pids/tids/jobids for each of our targets. We'll need handles to each of these
  // targets in order to suspend them and read their memory. We'll walk the root job tree looking
  // for anything that has a koid that matches the ones we've been given.
  TaskFinder finder;
  switch (request.config()->target()->Which()) {
    case fuchsia_cpu_profiler::TargetConfig::Tag::kTasks: {
      for (auto& t : request.config()->target()->tasks().value()) {
        switch (t.Which()) {
          case fuchsia_cpu_profiler::Task::Tag::kProcess:
            finder.AddProcess(t.process().value());
            break;
          case fuchsia_cpu_profiler::Task::Tag::kThread:
            finder.AddThread(t.thread().value());
            break;
          case fuchsia_cpu_profiler::Task::Tag::kJob:
            finder.AddJob(t.job().value());
            break;
          default:
            FX_LOGS(ERROR) << "Invalid task!";
            completer.Reply(
                fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
            return;
        }
      }
      break;
    }
    case fuchsia_cpu_profiler::TargetConfig::Tag::kComponent: {
      if (request.config()->target()->component()->url().has_value()) {
        std::string url = request.config()->target()->component()->url().value();
        std::string moniker;
        if (request.config()->target()->component()->moniker().has_value()) {
          moniker = request.config()->target()->component()->moniker().value();
        } else {
          // Default to core/ffx-laboratory
          const std::string parent_moniker = "./core";
          const std::string collection = "ffx-laboratory";

          // url: fuchsia-pkg://fuchsia.com/package#meta/component.cm
          const size_t name_start = url.find_last_of('/');
          const size_t name_end = url.find_last_of('.');
          if (name_start == std::string::npos || name_end == std::string::npos) {
            FX_LOGS(ERROR) << "Invalid url: " << url;
            completer.Reply(
                fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
            return;
          }
          // name: component
          const std::string name = url.substr(name_start + 1, name_end - (name_start + 1));
          moniker = "./core/ffx-laboratory:" + name;
        }

        zx::result<std::unique_ptr<profiler::Component>> res =
            profiler::Component::Create(dispatcher_, url, std::move(moniker));
        if (res.is_error()) {
          FX_PLOGS(INFO, res.error_value())
              << "No access to fuchsia.sys2.LifecycleController.root. Component launching and attaching is disabled";
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        component_target_ = std::move(*res);
      } else if (request.config()->target()->component()->moniker()) {
        // We've been asked to attach to an existing component by moniker. To do this, we ask
        // RealmQuery to get the component's RuntimeDirectory, then read that to get the component's
        // job id. Once we have the job id, we can add it to the TargetTree.
        zx::result<zx_koid_t> job_id =
            MonikerToJobId(*request.config()->target()->component()->moniker());
        if (job_id.is_error()) {
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        finder.AddJob(*job_id);
      } else {
        completer.Reply(
            fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
        return;
      }
      break;
    }
    case fuchsia_cpu_profiler::TargetConfig::Tag::kTest:
    default:
      completer.Reply(
          fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
      return;
  }

  if (!component_target_) {
    zx::result<TaskFinder::FoundTasks> handles_result = finder.FindHandles();
    if (handles_result.is_error()) {
      FX_PLOGS(ERROR, handles_result.error_value()) << "Failed to walk job tree";
      completer.Reply(
          fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
      return;
    }
    if (handles_result->empty()) {
      FX_LOGS(ERROR) << "Found no relevant handles";
      completer.Reply(
          fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
      return;
    }

    targets_.Clear();
    if (PopulateTargets(targets_, std::move(*handles_result)).is_error()) {
      completer.Reply(
          fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
      return;
    }
  }

  state_ = ProfilingState::Stopped;
  completer.Reply(fit::ok());
}

void profiler::ProfilerControllerImpl::Start(StartRequest& request,
                                             StartCompleter::Sync& completer) {
  if (state_ != ProfilingState::Stopped) {
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionStartError::kBadState));
    return;
  }

  sampler_ = std::make_unique<Sampler>(dispatcher_, std::move(targets_));
  targets_.Clear();
  if (component_target_) {
    if (zx::result<> res = component_target_->Start(); res.is_error()) {
      completer.Close(res.error_value());
      return;
    }
    std::string moniker = component_target_->Moniker();
    zx::result<zx_koid_t> job_id = MonikerToJobId(moniker);
    if (job_id.is_error()) {
      completer.Close(job_id.error_value());
      return;
    }
    TaskFinder tf;
    tf.AddJob(*job_id);
    zx::result<TaskFinder::FoundTasks> handles = tf.FindHandles();
    if (handles.is_error()) {
      FX_PLOGS(ERROR, handles.error_value()) << "Failed to find handle for: " << moniker;
      return;
    }
    for (auto& [koid, handle] : handles->jobs) {
      if (koid == job_id) {
        zx::result<JobTarget> target = MakeJobTarget(zx::job(handle.release()));
        if (target.is_error()) {
          FX_PLOGS(ERROR, target.status_value()) << "Failed to make target for: " << moniker;
          return;
        }
        zx::result<> target_result = sampler_->AddTarget(std::move(*target));
        if (target_result.is_error()) {
          FX_PLOGS(ERROR, target_result.error_value()) << "Failed to add target for: " << moniker;
          return;
        }
        break;
      }
    }
  };

  zx::result<> start_res = sampler_->Start();
  if (start_res.is_error()) {
    Reset();
    completer.Close(start_res.status_value());
    return;
  }
  state_ = ProfilingState::Running;
  completer.Reply(fit::ok());
}

void profiler::ProfilerControllerImpl::Stop(StopCompleter::Sync& completer) {
  zx::result<profiler::SymbolizationContext> modules = sampler_->GetContexts();
  if (modules.is_error()) {
    Reset();
    completer.Close(modules.status_value());
    return;
  }

  zx::result<> stop_res = sampler_->Stop();
  if (stop_res.is_error()) {
    FX_PLOGS(ERROR, stop_res.status_value()) << "Sampler failed to stop";
    Reset();
    completer.Close(stop_res.status_value());
    return;
  }

  if (component_target_) {
    if (zx::result<> res = component_target_->Stop(); res.is_error()) {
      FX_PLOGS(WARNING, res.error_value()) << "Failed to stop launched components";
    }

    if (zx::result<> res = component_target_->Destroy(); res.is_error()) {
      FX_PLOGS(WARNING, res.error_value()) << "Failed to destroy launched components";
    }
  }

  for (const auto& [pid, samples] : sampler_->GetSamples()) {
    if (!fsl::BlockingCopyFromString(profiler::symbolizer_markup::kReset, socket_)) {
      completer.Close(ZX_ERR_IO);
      return;
    }
    auto process_modules = modules->process_contexts[pid];
    for (const profiler::Module& mod : process_modules) {
      if (!fsl::BlockingCopyFromString(profiler::symbolizer_markup::FormatModule(mod), socket_)) {
        completer.Close(ZX_ERR_IO);
        return;
      }
    }
    for (const Sample& sample : samples) {
      if (!fsl::BlockingCopyFromString(profiler::symbolizer_markup::FormatSample(sample),
                                       socket_)) {
        completer.Close(ZX_ERR_IO);
        return;
      }
    }
  }

  socket_.reset();

  std::vector<zx::ticks> inspecting_durations = sampler_->SamplingDurations();
  fuchsia_cpu_profiler::SessionStopResponse stats;
  stats.samples_collected() = inspecting_durations.size();
  if (!inspecting_durations.empty()) {
    zx::ticks total_ticks;
    for (zx::ticks ticks : inspecting_durations) {
      total_ticks += ticks;
    }
    auto ticks_per_second = zx::ticks::per_second();
    auto ticks_per_us = ticks_per_second / 1000000;

    zx::ticks total_ticks_inspecting;
    for (zx::ticks ticks : inspecting_durations) {
      total_ticks_inspecting += ticks;
    }
    std::sort(inspecting_durations.begin(), inspecting_durations.end(),
              [](zx::ticks a, zx::ticks b) { return a < b; });
    zx::ticks mean_inspecting = total_ticks_inspecting / inspecting_durations.size();

    stats.mean_sample_time() = mean_inspecting / ticks_per_us;
    stats.median_sample_time() =
        inspecting_durations[inspecting_durations.size() / 2] / ticks_per_us;
    stats.min_sample_time() = inspecting_durations.front() / ticks_per_us;
    stats.max_sample_time() = inspecting_durations.back() / ticks_per_us;
  }
  Reset();
  completer.Reply(std::move(stats));
}

void profiler::ProfilerControllerImpl::Reset(ResetCompleter::Sync& completer) {
  Reset();
  completer.Reply();
}

void profiler::ProfilerControllerImpl::OnUnbound(
    fidl::UnbindInfo info, fidl::ServerEnd<fuchsia_cpu_profiler::Session> server_end) {
  Reset();
}

void profiler::ProfilerControllerImpl::Reset() {
  sampler_.reset();
  socket_.reset();
  targets_.Clear();
  state_ = ProfilingState::Unconfigured;
}
