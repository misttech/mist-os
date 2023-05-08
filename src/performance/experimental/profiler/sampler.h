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
#include <thread>
#include <vector>

#include <inspector/inspector.h>
#include <src/lib/unwinder/cfi_unwinder.h>
#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/fuchsia.h>
#include <src/lib/unwinder/unwind.h>

struct SamplingInfo {
  SamplingInfo(zx::process process, std::vector<zx::thread> threads, unwinder::CfiUnwinder unwinder)
      : process(std::move(process)),
        threads(std::move(threads)),
        cfi_unwinder(std::move(unwinder)),
        fp_unwinder(&cfi_unwinder) {}
  SamplingInfo(SamplingInfo&&) = default;
  SamplingInfo& operator=(SamplingInfo&&) = default;

  zx::process process;
  std::vector<zx::thread> threads;
  unwinder::CfiUnwinder cfi_unwinder;
  unwinder::FramePointerUnwinder fp_unwinder;
};

class Sampler {
 public:
  explicit Sampler(std::vector<SamplingInfo> targets) : targets_(std::move(targets)) {}

  zx::result<> Start();
  zx::result<> Stop();

  void PrintMarkupContext(FILE* f) {
    std::lock_guard lock(data_lock_);
    for (const SamplingInfo& target : targets_) {
      inspector_print_markup_context(f, target.process.get());
    }
  }

  std::vector<std::vector<uint64_t>> GetStacks() {
    std::lock_guard lock(data_lock_);
    return stacks_;
  }
  std::vector<zx::ticks> SamplingDurations() {
    std::lock_guard lock(data_lock_);
    return inspecting_durations_;
  }

 private:
  void CollectSamples();
  enum State {
    Running,
    Stopping,
    Stopped,
  };
  std::mutex state_lock_;
  std::mutex data_lock_;

  std::atomic<State> state_ = State::Stopped;
  std::condition_variable state_cv_;

  std::vector<SamplingInfo> targets_ __TA_GUARDED(data_lock_);
  std::thread collection_thread_ __TA_GUARDED(data_lock_);
  std::vector<zx::ticks> inspecting_durations_ __TA_GUARDED(data_lock_);
  std::vector<std::vector<uint64_t>> stacks_ __TA_GUARDED(data_lock_);
};
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SAMPLER_H_
