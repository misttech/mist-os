// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLER_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLER_H_

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/task.h>
#include <lib/zx/thread.h>
#include <lib/zxdump/elf-search.h>
#include <zircon/compiler.h>

#include <condition_variable>
#include <mutex>
#include <vector>

#include <inspector/inspector.h>
#include <src/lib/unwinder/cfi_unwinder.h>
#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/fuchsia.h>
#include <src/lib/unwinder/unwind.h>

#include "process_watcher.h"

struct SamplingInfo {
  SamplingInfo(zx::process process, zx_koid_t pid,
               std::vector<std::pair<zx_koid_t, zx::thread>> threads,
               unwinder::CfiUnwinder unwinder)
      : process(std::move(process)),
        pid(pid),
        threads(std::move(threads)),
        cfi_unwinder(std::move(unwinder)),
        fp_unwinder(&cfi_unwinder) {}
  SamplingInfo(SamplingInfo&&) = default;
  SamplingInfo& operator=(SamplingInfo&&) = default;

  zx::process process;
  zx_koid_t pid;
  std::vector<std::pair<zx_koid_t, zx::thread>> threads;
  unwinder::CfiUnwinder cfi_unwinder;
  unwinder::FramePointerUnwinder fp_unwinder;
};

struct Sample {
  zx_koid_t pid;
  zx_koid_t tid;
  std::vector<uint64_t> stack;
};

class Sampler {
 public:
  explicit Sampler(async_dispatcher_t* dispatcher, std::vector<SamplingInfo> targets)
      : dispatcher_(dispatcher), targets_(std::move(targets)) {}

  zx::result<> Start();
  zx::result<> Stop();

  void PrintMarkupContext(FILE* f) {
    for (const SamplingInfo& target : targets_) {
      inspector_print_markup_context(f, target.process.get());
    }
  }

  std::vector<Sample> GetSamples() { return samples_; }
  std::vector<zx::ticks> SamplingDurations() { return inspecting_durations_; }

 private:
  void AddThread(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                 const zx_packet_signal_t* signal) {}

  void CollectSamples();
  enum State {
    Running,
    Stopped,
  };

  async_dispatcher_t* dispatcher_;

  std::atomic<State> state_ = State::Stopped;
  std::condition_variable state_cv_;

  std::vector<SamplingInfo> targets_;
  std::vector<zx::ticks> inspecting_durations_;
  std::vector<Sample> samples_;
  std::vector<std::unique_ptr<ProcessWatcher>> watchers_;
};
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLER_H_
