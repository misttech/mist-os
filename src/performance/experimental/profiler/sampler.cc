// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sampler.h"

#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/stdcompat/span.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/suspend_token.h>
#include <lib/zx/thread.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <map>
#include <memory>
#include <unordered_map>
#include <utility>
#include <vector>

#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/registers.h>
#include <src/lib/unwinder/unwind.h>

#include "job_watcher.h"
#include "process_watcher.h"
#include "symbolization_context.h"
#include "targets.h"

std::pair<zx::ticks, std::vector<uint64_t>> SampleThread(const zx::unowned_process& process,
                                                         const zx::unowned_thread& thread,
                                                         unwinder::FramePointerUnwinder& unwinder) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  zx_info_thread_t thread_info;
  zx_status_t status =
      thread->get_info(ZX_INFO_THREAD, &thread_info, sizeof(thread_info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "unable to get thread info for thread " << thread->get()
                            << ", skipping";
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }

  // Skip threads that are not actively running or blocked
  if (!((thread_info.state == ZX_THREAD_STATE_RUNNING) ||
        (thread_info.state & ZX_THREAD_STATE_BLOCKED))) {
    return {zx::ticks(), std::vector<uint64_t>()};
  }

  zx::ticks before = zx::ticks::now();
  zx::suspend_token suspend_token;
  status = thread->suspend(&suspend_token);
  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "Failed to suspend thread: " << thread->get();
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }

  // Asking to wait for suspended means only waiting for the thread to suspend. If the thread
  // terminates instead this will wait forever (or until the timeout). Thus we need to explicitly
  // wait for ZX_THREAD_TERMINATED too.
  zx_signals_t signals = ZX_THREAD_SUSPENDED | ZX_THREAD_TERMINATED;
  zx_signals_t observed = 0;
  zx::time deadline = zx::deadline_after(zx::msec(100));
  status = thread->wait_one(signals, deadline, &observed);

  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "failure waiting for thread to suspend, skipping thread: "
                              << thread->get();
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }

  if (observed & ZX_THREAD_TERMINATED) {
    FX_LOGS(INFO) << "Skipping terminated thread...";
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }
  unwinder::FuchsiaMemory memory(process->get());

  // Setup registers.
  zx_thread_state_general_regs_t regs;
  if (thread->read_state(ZX_THREAD_STATE_GENERAL_REGS, &regs, sizeof(regs)) != ZX_OK) {
    FX_LOGS(INFO) << "Can't read state, Skipping thread...";
    return {zx::ticks(), std::vector<uint64_t>()};
  }
  auto registers = unwinder::FromFuchsiaRegisters(regs);

  std::vector<uint64_t> pcs;
  pcs.reserve(50);
  registers.GetPC(pcs.emplace_back());
  unwinder::Frame current{registers, /*pc_is_return_address=*/true,
                          unwinder::Frame::Trust::kContext};
  for (size_t i = 0; i < 50; i++) {
    unwinder::Frame next(unwinder::Registers{current.regs.arch()},
                         /*pc_is_return_address=*/false,
                         /*placeholder*/ unwinder::Frame::Trust::kFP);

    bool success = unwinder.Step(&memory, current.regs, next.regs).ok();

    // An undefined PC (e.g. on Linux) or 0 PC (e.g. on Fuchsia) marks the end of the unwinding.
    // Don't include this in the output because it's not a real frame and provides no information.
    // A failed unwinding will also end up with an undefined PC.
    uint64_t pc;
    if (!success || next.regs.GetPC(pc).has_err() || pc == 0) {
      break;
    }
    pcs.push_back(pc);
    current = next;
  }
  zx::ticks duration = zx::ticks::now() - before;
  return {duration, pcs};
}

zx::result<> profiler::Sampler::AddTarget(JobTarget&& target) {
  zx::result<> res = WatchTarget(target);
  if (res.is_error()) {
    return res;
  }

  return targets_.AddJob(std::move(target));
}

zx::result<> profiler::Sampler::WatchTarget(const JobTarget& target) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  std::vector<zx_koid_t> job_path = target.ancestry;
  job_path.push_back(target.job_id);
  auto job_watcher = std::make_unique<JobWatcher>(
      target.job.borrow(), [job_path = std::move(job_path), this](zx_koid_t pid, zx::process p) {
        // We've intercepted this process before its threads have started, so we don't recursively
        // add them here. We let the watcher handle the thread start exceptions as soon as we
        // acknowledge this process start exception.
        ProcessTarget process_target =
            ProcessTarget{std::move(p), pid, std::unordered_map<zx_koid_t, ThreadTarget>()};
        // Furthermore, we need to watch each started process for threads it creates
        auto process_watcher = std::make_unique<ProcessWatcher>(
            process_target.handle.borrow(),
            [job_path, this](zx_koid_t pid, zx_koid_t tid, zx::thread t) {
              AddThread(job_path, pid, tid, std::move(t));
            },
            [job_path, this](zx_koid_t pid, zx_koid_t tid) { RemoveThread(job_path, pid, tid); }

        );
        auto [it, emplaced] = process_watchers_.emplace(pid, std::move(process_watcher));
        if (emplaced) {
          if (zx::result watch_result = it->second->Watch(dispatcher_); watch_result.is_error()) {
            if (watch_result.error_value() == ZX_ERR_BAD_STATE) {
              FX_LOGS(DEBUG) << "Process terminated before being watched.";
            } else {
              FX_PLOGS(ERROR, watch_result.status_value()) << "Failed to watch process: " << pid;
              job_watchers_.clear();
              process_watchers_.clear();
              return;
            }
          }
        }

        if (zx::result res = targets_.AddProcess(job_path, std::move(process_target));
            res.is_error()) {
          FX_PLOGS(ERROR, res.status_value()) << "Failed to add process to session: " << pid;
        }
      });

  auto [it, emplaced] = job_watchers_.emplace(target.job_id, std::move(job_watcher));
  if (emplaced) {
    if (zx::result res = it->second->Watch(dispatcher_); res.is_error()) {
      if (res.error_value() == ZX_ERR_BAD_STATE) {
        FX_LOGS(DEBUG) << "Job terminated before being watched.";
      } else {
        FX_PLOGS(ERROR, res.status_value()) << "Failed to watch job : " << target.job_id;
        job_watchers_.clear();
        return res;
      }
    }
  }
  return zx::ok();
}

zx::result<> profiler::Sampler::Start(size_t buffer_size_mb /* unused, we buffer in memory */) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  // When we start, we want to recursively attach to each process and job we've found. In addition,
  // we want to set up a notification for new jobs and processes so that if any watched task creates
  // a sub task, we also attach to it and watch it.
  //
  // A job or process may have exited between us configuring our target tree and actually starting,
  // so if we find we're unable to attach to a task, we simply move on without it.
  zx::result<> res = targets_.ForEachProcess([this](cpp20::span<const zx_koid_t> job_path,
                                                    const ProcessTarget& p) -> zx::result<> {
    TRACE_DURATION("cpu_profiler", "Sampler::Start/ForEachProcess");
    std::vector<zx_koid_t> saved_path{job_path.begin(), job_path.end()};
    auto process_watcher = std::make_unique<ProcessWatcher>(
        p.handle.borrow(),
        [saved_path, this](zx_koid_t pid, zx_koid_t tid, zx::thread t) {
          zx::result res =
              targets_.AddThread(saved_path, pid, ThreadTarget{.handle = std::move(t), .tid = tid});
          if (res.is_error()) {
            FX_PLOGS(ERROR, res.status_value())
                << "Failed to add thread: " << tid << " pid: " << pid;
          }
        },
        [saved_path, this](zx_koid_t pid, zx_koid_t tid) {
          zx::result res = targets_.RemoveThread(saved_path, pid, tid);
          if (res.is_error()) {
            FX_PLOGS(ERROR, res.status_value())
                << "Failed to remove thread: " << tid << " pid: " << pid;
          }
        });

    auto [it, emplaced] = process_watchers_.emplace(p.pid, std::move(process_watcher));
    if (emplaced) {
      if (zx::result watch_result = it->second->Watch(dispatcher_); watch_result.is_error()) {
        // A watch may fail for a number of reason, possibly the process exited out from under us.
        // Simply move on without it and attempt to watch the remaining processes.
        FX_PLOGS(WARNING, watch_result.status_value()) << "Failed to watch process: " << p.pid;
      }
    }
    return zx::ok();
  });

  if (res.is_error()) {
    return res;
  }

  // If we watched job launches a new process, we want to add it to the set.
  if (auto res = targets_.ForEachJob([this](const JobTarget& target) {
        if (zx::result res = WatchTarget(target); res.is_error()) {
          // If a job exited out from underneath us, simply move on without it.
          FX_PLOGS(WARNING, res.status_value())
              << "Failed to watch job [" << target.job_id << "] and its children";
        }
        return zx::ok();
      });
      res.is_error()) {
    return res;
  }

  inspecting_durations_.reserve(1000);
  samples_.reserve(1000);
  sample_task_.Post(dispatcher_);
  return zx::ok();
}

zx::result<> profiler::Sampler::Stop() {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  sample_task_.Cancel();
  FX_LOGS(INFO) << "Stopped! Collected " << inspecting_durations_.size() << " samples";
  sample_task_.Cancel();
  return zx::ok();
}

void profiler::Sampler::CollectSamples(async_dispatcher_t* dispatcher, async::TaskBase* task,
                                       zx_status_t status) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  if (status != ZX_OK) {
    return;
  }

  zx::result res =
      targets_.ForEachProcess([this](cpp20::span<const zx_koid_t>, const ProcessTarget& target) {
        for (const auto& [_, thread] : target.threads) {
          TRACE_DURATION("cpu_profiler", "Sampler::CollectSamples/ForEachProcess");
          unwinder::CfiUnwinder cfi_unwinder{target.unwinder_data->modules};
          unwinder::FramePointerUnwinder fp_unwinder{&cfi_unwinder};
          auto [time_sampling, pcs] =
              SampleThread(target.handle.borrow(), thread.handle.borrow(), fp_unwinder);
          if (time_sampling != zx::ticks()) {
            samples_[target.pid].push_back({target.pid, thread.tid, pcs});
            inspecting_durations_.push_back(time_sampling);
          }
        }
        return zx::ok();
      });
  if (res.is_error()) {
    FX_PLOGS(ERROR, res.status_value()) << "Sampling Failed";
    return;
  }

  sample_task_.PostDelayed(dispatcher_, zx::msec(10));
}

zx::result<profiler::SymbolizationContext> profiler::Sampler::GetContexts() {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  std::map<zx_koid_t, std::vector<profiler::Module>> contexts;
  zx::result<> res = targets_.ForEachProcess(
      [&contexts, this](cpp20::span<const zx_koid_t>,
                        const ProcessTarget& target) mutable -> zx::result<> {
        zx::result<std::vector<profiler::Module>> modules =
            targets_.GetProcessModules(target.handle);
        if (modules.is_ok()) {
          contexts[target.pid] = *modules;
        }
        // It's possible that the process we were profiling no longer exists -- it exited before the
        // profile ended. If this happens, we don't want ForEachProcess to short circuit and stop,
        // so we return success even if we "failed".
        return zx::ok();
      });
  if (res.is_error()) {
    return res.take_error();
  }
  return zx::ok(profiler::SymbolizationContext{contexts});
}

void profiler::Sampler::AddThread(std::vector<zx_koid_t> job_path, zx_koid_t pid, zx_koid_t tid,
                                  zx::thread t) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  zx::result res = targets_.AddThread(job_path, pid, ThreadTarget{std::move(t), tid});
  if (res.is_error()) {
    FX_PLOGS(ERROR, res.status_value()) << "Failed to add thread to session: " << tid;
  }
}

void profiler::Sampler::RemoveThread(std::vector<zx_koid_t> job_path, zx_koid_t pid,
                                     zx_koid_t tid) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  zx::result res = targets_.RemoveThread(job_path, pid, tid);
  if (res.is_error()) {
    FX_PLOGS(ERROR, res.status_value()) << "Failed to remove exited thread: " << tid;
  }
}
