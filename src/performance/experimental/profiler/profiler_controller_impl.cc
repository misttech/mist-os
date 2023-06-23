// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "profiler_controller_impl.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>

#include <src/lib/fsl/socket/strings.h>
#include <src/lib/unwinder/cfi_unwinder.h>
#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/fuchsia.h>
#include <src/lib/unwinder/unwind.h>

#include "sampler.h"
#include "targets.h"
#include "taskfinder.h"
#include "zircon/system/ulib/elf-search/include/elf-search.h"

void profiler::ProfilerControllerImpl::Configure(ConfigureRequest& request,
                                                 ConfigureCompleter::Sync& completer) {
  std::lock_guard lock(state_lock_);
  if (state_ != ProfilingState::Unconfigured) {
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kBadState));
    return;
  }
  if (!request.output()) {
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kBadSocket));
    return;
  }
  socket_ = std::move(*request.output());

  if (!request.config() || !request.config()->targets() || request.config()->targets()->empty()) {
    FX_LOGS(ERROR) << "No Targets Specified and System Wide profiling isn't yet implemented!";
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
    return;
  }

  // We're given pids/tids/jobids for each of our targets. We'll need handles to each of these
  // targets in order to suspend them and read their memory. We'll walk the root job tree looking
  // for anything that has a koid that matches the ones we've been given.
  TaskFinder finder;
  for (auto& i : *request.config()->targets()) {
    if (!i.task()) {
      FX_LOGS(ERROR) << "Target Config Missing task!";
      completer.Reply(
          fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
      return;
    }
    if (i.task()->job()) {
      finder.AddJob(i.task()->job().value());
    }
    if (i.task()->thread()) {
      finder.AddThread(i.task()->thread().value());
    }
    if (i.task()->process()) {
      finder.AddProcess(i.task()->process().value());
    }
  }

  zx::result<std::vector<std::pair<zx_koid_t, zx::handle>>> handles_result = finder.FindHandles();
  if (handles_result.is_error()) {
    FX_PLOGS(ERROR, handles_result.error_value()) << "Failed to walk job tree";
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
    return;
  }
  if (handles_result->empty()) {
    FX_LOGS(ERROR) << "Found " << handles_result->size() << " relevant handles";
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
    return;
  }

  targets_.clear();
  for (auto&& [koid, handle] : *handles_result) {
    zx_info_handle_basic_t info;
    zx_status_t res = handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
    if (res != ZX_OK) {
      FX_PLOGS(ERROR, res) << "Failed to get info about handle: " << handle.get();
      completer.Reply(
          fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
      return;
    }
    switch (info.type) {
      case ZX_OBJ_TYPE_PROCESS: {
        zx::process process{std::move(handle)};
        zx_info_handle_basic_t handle_info;
        zx_status_t res = process.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info),
                                           nullptr, nullptr);
        if (res != ZX_OK) {
          FX_PLOGS(ERROR, res) << "Failed to get info about handle: " << handle.get();
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        zx::result<ProcessTarget> process_target = MakeProcessTarget(std::move(process));
        if (process_target.is_error()) {
          FX_PLOGS(ERROR, res) << "Failed to make a process target";
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        JobTarget job_target{handle_info.related_koid};
        job_target.processes.push_back(std::move(*process_target));
        targets_.push_back(std::move(job_target));
        break;
      }
      case ZX_OBJ_TYPE_JOB: {
        zx::result<JobTarget> job_target = MakeJobTarget(zx::job{std::move(handle)});
        if (job_target.is_error()) {
          FX_PLOGS(ERROR, res) << "Failed to make a job target";
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        targets_.push_back(std::move(*job_target));
        break;
      }
      case ZX_OBJ_TYPE_THREAD: {
        // If we have a thread, we need to know what its parent process is
        // and then get a handle to it. Unfortunately, the "easiest" way to
        // do this is to walk the job tree again.
        zx::thread thread{std::move(handle)};
        TaskFinder finder;
        finder.AddProcess(info.related_koid);
        zx::result<std::vector<std::pair<zx_koid_t, zx::handle>>> handles_result =
            finder.FindHandles();
        if (handles_result.is_error()) {
          FX_PLOGS(ERROR, handles_result.error_value()) << "Failed to walk job tree";
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        if (handles_result->size() != 1) {
          FX_LOGS(ERROR) << "Found the wrong number of processes for thread: " << thread.get();
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        auto [pid, process] = std::move((*handles_result)[0]);

        zx_info_handle_basic_t handle_info;
        zx_status_t res = process.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info),
                                           nullptr, nullptr);
        if (res != ZX_OK) {
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }

        std::vector<ThreadTarget> threads;
        threads.push_back(ThreadTarget{std::move(thread), koid});
        ProcessTarget process_target{zx::process{std::move(process)}, koid, std::move(threads)};
        elf_search::ForEachModule(*zx::unowned_process{process.get()},
                                  [&process_target](const elf_search::ModuleInfo& info) {
                                    process_target.unwinder_data->modules.emplace_back(
                                        info.vaddr, &process_target.unwinder_data->memory,
                                        unwinder::Module::AddressMode::kProcess);
                                  });
        JobTarget job_target{handle_info.related_koid};
        job_target.processes.push_back(std::move(process_target));
        targets_.push_back(std::move(job_target));
        break;
      }
      default:
        FX_LOGS(ERROR) << "Got unknown handle_type: " << info.type;
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
  std::lock_guard lock(state_lock_);
  if (state_ != ProfilingState::Stopped) {
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionStartError::kBadState));
    return;
  }

  FX_LOGS(INFO) << "Starting Collection for " << targets_.size() << " targets";
  sampler_ = std::make_unique<Sampler>(dispatcher_, std::move(targets_));
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
  std::lock_guard lock(state_lock_);
  zx::result<> stop_res = sampler_->Stop();
  if (stop_res.is_error()) {
    FX_PLOGS(ERROR, stop_res.status_value()) << "Sampler failed to stop";
    Reset();
    completer.Close(stop_res.status_value());
    return;
  }

  // Start by writing out symbolization information to the sockets
  zx::result<profiler::SymbolizationContext> modules = sampler_->GetContexts();
  if (modules.is_error()) {
    completer.Close(modules.status_value());
  }

  std::stringstream modules_ss;
  modules_ss << "{{{reset}}}\n";
  for (const auto& [pid, modules] : modules->process_contexts) {
    const size_t kPageSize = zx_system_get_page_size();
    for (const profiler::Module& mod : modules) {
      modules_ss << "{{{module:0x" << std::hex << mod.module_id << std::dec << ":"
                 << mod.module_name << ":elf:" << mod.build_id << "}}}\n";
      for (const profiler::Segment& segment : mod.loads) {
        uintptr_t start = segment.p_vaddr & -kPageSize;
        uintptr_t end = (segment.p_vaddr + segment.p_memsz + kPageSize - 1) & -kPageSize;
        modules_ss << "{{{mmap:0x" << std::hex << mod.vaddr + start << ":0x" << end - start
                   << ":load:" << mod.module_id << ":";

        if (segment.p_flags & PF_R) {
          modules_ss << 'r';
        }
        if (segment.p_flags & PF_W) {
          modules_ss << 'w';
        }
        if (segment.p_flags & PF_X) {
          modules_ss << 'x';
        }
        modules_ss << ":0x" << start << "}}}\n";
      }
    }
  }
  if (!fsl::BlockingCopyFromString(modules_ss.str(), socket_)) {
    completer.Close(ZX_ERR_IO);
    return;
  }

  // Then write out each sample
  for (const Sample& sample : sampler_->GetSamples()) {
    std::stringstream ss;
    ss << sample.pid << "\n" << sample.tid << "\n";
    int n = 0;
    for (const auto& frame : sample.stack) {
      const char* address_type = "ra";
      if (n == 0) {
        address_type = "pc";
      }
      ss << "{{{bt:" << n++ << ":0x" << std::hex << frame << std::dec << ":" << address_type
         << "}}}\n";
      if (!fsl::BlockingCopyFromString(ss.str(), socket_)) {
        completer.Close(ZX_ERR_IO);
        return;
      }
    }
  }

  socket_.reset();

  std::vector<zx::ticks> inspecting_durations = sampler_->SamplingDurations();
  fuchsia_cpu_profiler::SessionStopResponse stats;
  stats.samples_collected() = sampler_->GetSamples().size();
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
  std::lock_guard lock(state_lock_);
  Reset();
  completer.Reply();
}

void profiler::ProfilerControllerImpl::OnUnbound(
    fidl::UnbindInfo info, fidl::ServerEnd<fuchsia_cpu_profiler::Session> server_end) {
  std::lock_guard lock(state_lock_);
  Reset();
}

void profiler::ProfilerControllerImpl::Reset() {
  sampler_.reset();
  socket_.reset();
  targets_ = std::vector<JobTarget>();
  state_ = ProfilingState::Unconfigured;
}
