// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "profiler_controller_impl.h"

#include <lib/syslog/cpp/macros.h>

#include <inspector/inspector.h>
#include <src/lib/unwinder/cfi_unwinder.h>
#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/fuchsia.h>
#include <src/lib/unwinder/unwind.h>

#include "sampler.h"
#include "taskfinder.h"
#include "zircon/system/ulib/elf-search/include/elf-search.h"

zx::result<std::vector<zx_koid_t>> GetChildrenTids(const zx::process& process) {
  size_t num_threads;
  zx_status_t status = process.get_info(ZX_INFO_PROCESS_THREADS, nullptr, 0, nullptr, &num_threads);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to get process thread info (#threads)";
    return zx::error(status);
  }
  if (num_threads < 1) {
    FX_LOGS(ERROR) << "failed to get any threads associated with the process";
    return zx::error(ZX_ERR_BAD_STATE);
  }

  zx_koid_t threads[num_threads];
  size_t records_read;
  status = process.get_info(ZX_INFO_PROCESS_THREADS, threads, num_threads * sizeof(threads[0]),
                            &records_read, nullptr);

  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to get process thread info";
    return zx::error(status);
  }

  if (records_read != num_threads) {
    FX_LOGS(ERROR) << "records_read != num_threads";
    return zx::error(ZX_ERR_BAD_STATE);
  }

  std::vector<zx_koid_t> children{threads, threads + num_threads};
  return zx::ok(children);
}

void ProfilerControllerImpl::Configure(ConfigureRequest& request,
                                       ConfigureCompleter::Sync& completer) {
  FX_LOGS(INFO) << "Configure!";
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

  zx::result<std::vector<zx::handle>> handles_result = finder.FindHandles();
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
  for (auto&& handle : *handles_result) {
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
        zx::result<std::vector<zx_koid_t>> children = GetChildrenTids(process);
        if (children.is_error()) {
          completer.Reply(
              fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
          return;
        }
        std::vector<zx::thread> threads;
        for (auto child : *children) {
          zx::thread child_thread;
          zx_status_t res = process.get_child(child, ZX_DEFAULT_THREAD_RIGHTS, &child_thread);
          if (res != ZX_OK) {
            FX_PLOGS(ERROR, res) << "Failed to get handle for child (koid: " << child << ")";
            continue;
          }
          threads.push_back(std::move(child_thread));
        }

        unwinder::FuchsiaMemory memory(process.get());
        std::vector<unwinder::Module> modules;
        elf_search::ForEachModule(
            *zx::unowned_process{process}, [&modules, &memory](const elf_search::ModuleInfo& info) {
              modules.emplace_back(info.vaddr, &memory, unwinder::Module::AddressMode::kProcess);
            });
        unwinder::CfiUnwinder cfi_unwinder{modules};
        targets_.emplace_back(std::move(process), std::move(threads), std::move(cfi_unwinder));
        break;
      }
      case ZX_OBJ_TYPE_JOB:
        FX_LOGS(ERROR) << "Jobs are not yet supported";
        completer.Reply(
            fit::error(fuchsia_cpu_profiler::SessionConfigureError::kInvalidConfiguration));
        break;
      case ZX_OBJ_TYPE_THREAD: {
        // If we have a thread, we need to know what its parent process is
        // and then get a handle to it. Unfortunately, the "easiest" way to
        // do this is to walk the job tree again.
        zx::thread thread{std::move(handle)};
        TaskFinder finder;
        finder.AddProcess(info.related_koid);
        zx::result<std::vector<zx::handle>> handles_result = finder.FindHandles();
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
        zx::process process{std::move((*handles_result)[0])};

        std::vector<unwinder::Module> modules;
        unwinder::FuchsiaMemory memory(process.get());
        elf_search::ForEachModule(
            *zx::unowned_process{process}, [&modules, &memory](const elf_search::ModuleInfo& info) {
              modules.emplace_back(info.vaddr, &memory, unwinder::Module::AddressMode::kProcess);
            });
        unwinder::CfiUnwinder cfi_unwinder{modules};
        std::vector<zx::thread> threads;
        threads.push_back(std::move(thread));
        targets_.emplace_back(std::move(process), std::move(threads), std::move(cfi_unwinder));
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
  FX_LOGS(INFO) << "/Configure!";
}

void ProfilerControllerImpl::Start(StartRequest& request, StartCompleter::Sync& completer) {
  FX_LOGS(INFO) << "Start!";
  std::lock_guard lock(state_lock_);
  if (state_ != ProfilingState::Stopped) {
    completer.Reply(fit::error(fuchsia_cpu_profiler::SessionStartError::kBadState));
    return;
  }

  FX_LOGS(INFO) << "Starting Collection for " << targets_.size() << " targets";
  sampler_ = std::make_unique<Sampler>(std::move(targets_));
  zx::result<> start_res = sampler_->Start();
  if (start_res.is_error()) {
    Reset();
    completer.Close(start_res.status_value());
    return;
  }
  state_ = ProfilingState::Running;
  completer.Reply(fit::ok());
  FX_LOGS(INFO) << "/Start!";
}

void ProfilerControllerImpl::Stop(StopCompleter::Sync& completer) {
  std::lock_guard lock(state_lock_);
  zx::result<> stop_res = sampler_->Stop();
  if (stop_res.is_error()) {
    FX_PLOGS(ERROR, stop_res.status_value()) << "Sampler failed to stop";
    Reset();
    completer.Close(stop_res.status_value());
    return;
  }

  FILE* f = tmpfile();
  if (f == nullptr) {
    FX_LOGS(ERROR) << "failed to open file: " << errno;
    fclose(f);
    Reset();
    completer.Close(ZX_ERR_BAD_PATH);
    return;
  }
  sampler_->PrintMarkupContext(f);
  for (const auto& stack : sampler_->GetStacks()) {
    int n = 0;
    for (const auto& frame : stack) {
      const char* address_type = "ra";
      if (n == 0) {
        address_type = "pc";
      }
      fprintf(f, "{{{bt:%u:%#" PRIxPTR ":%s}}}\n", n++, frame, address_type);
    }
  }
  fflush(f);
  if (fseek(f, 0, SEEK_SET)) {
    FX_LOGS(ERROR) << "Failed to rewind tmp file: " << errno;
    Reset();
    completer.Close(ZX_ERR_BAD_PATH);
    return;
  }

  size_t total = 0;
  for (;;) {
    uint8_t buffer[1024];
    size_t count = fread(buffer, sizeof(buffer[0]), sizeof(buffer) / sizeof(buffer[0]), f);
    if (count <= 0) {
      break;
    }
    size_t remaining = count;
    while (remaining > 0) {
      size_t written;
      zx_status_t res = socket_.write(0, buffer, remaining, &written);
      total += written;
      remaining -= written;
      if (res == ZX_ERR_SHOULD_WAIT) {
        zx_signals_t observed;
        socket_.wait_one(ZX_SOCKET_WRITABLE, zx::time::infinite(), &observed);
        if (!(observed & ZX_SOCKET_WRITABLE)) {
          break;
        }
      } else if (res != ZX_OK) {
        break;
      }
    }
  }

  socket_.reset();
  fclose(f);
  FX_LOGS(INFO) << "Wrote " << total << " bytes to socket";

  std::vector<zx::ticks> inspecting_durations = sampler_->SamplingDurations();
  fuchsia_cpu_profiler::SessionStopResponse stats;
  stats.samples_collected() = sampler_->GetStacks().size();
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

void ProfilerControllerImpl::Reset(ResetCompleter::Sync& completer) {
  std::lock_guard lock(state_lock_);
  Reset();
  completer.Reply();
}

void ProfilerControllerImpl::OnUnbound(fidl::UnbindInfo info,
                                       fidl::ServerEnd<fuchsia_cpu_profiler::Session> server_end) {
  std::lock_guard lock(state_lock_);
  Reset();
}

void ProfilerControllerImpl::Reset() {
  sampler_.reset();
  socket_.reset();
  targets_ = std::vector<SamplingInfo>();
  state_ = ProfilingState::Unconfigured;
}
