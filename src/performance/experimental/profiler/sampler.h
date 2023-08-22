// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLER_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLER_H_

#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/task.h>
#include <lib/zx/thread.h>
#include <lib/zxdump/elf-search.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <memory>
#include <unordered_map>
#include <vector>

#include <src/lib/unwinder/cfi_unwinder.h>
#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/fuchsia.h>
#include <src/lib/unwinder/unwind.h>

#include "job_watcher.h"
#include "process_watcher.h"
#include "symbolization_context.h"
#include "targets.h"

namespace profiler {
struct Sample {
  zx_koid_t pid;
  zx_koid_t tid;
  std::vector<uint64_t> stack;
};

class Sampler {
 public:
  explicit Sampler(async_dispatcher_t* dispatcher, TargetTree&& targets)
      : dispatcher_(dispatcher), targets_(std::move(targets)) {}

  zx::result<> Start();
  zx::result<> Stop();

  // Return the information needed to symbolize the samples
  zx::result<profiler::SymbolizationContext> GetContexts();

  std::unordered_map<zx_koid_t, std::vector<Sample>> GetSamples() { return samples_; }
  std::vector<zx::ticks> SamplingDurations() { return inspecting_durations_; }
  zx::result<> AddTarget(JobTarget&& target);

 private:
  zx::result<> WatchTarget(const JobTarget& target);
  void AddThread(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                 const zx_packet_signal_t* signal) {}

  void CollectSamples(async_dispatcher_t* dispatcher, async::TaskBase* task, zx_status_t status);

  async_dispatcher_t* dispatcher_;
  async::TaskMethod<profiler::Sampler, &profiler::Sampler::CollectSamples> sample_task_{this};

  TargetTree targets_;
  std::vector<zx::ticks> inspecting_durations_;
  std::unordered_map<zx_koid_t, std::vector<Sample>> samples_;

  // Watchers cannot be moved, so we need to box them
  std::unordered_map<zx_koid_t, std::unique_ptr<ProcessWatcher>> process_watchers_;
  std::unordered_map<zx_koid_t, std::unique_ptr<JobWatcher>> job_watchers_;
};
}  // namespace profiler
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLER_H_
