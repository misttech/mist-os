// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_TARGETS_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_TARGETS_H_

#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/thread.h>

#include <memory>
#include <vector>

#include <src/lib/unwinder/cfi_unwinder.h>
#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/fuchsia.h>
#include <src/lib/unwinder/unwind.h>

namespace profiler {
struct ThreadTarget {
  zx::thread handle;
  zx_koid_t tid;
};

// The unwinding library receives pointers and references. We place the relevant structs together to
// ensure they don't get copied/moved and invalidate the references.
struct UnwinderData {
  explicit UnwinderData(const zx::unowned_process& process)
      : memory(process->get()), cfi_unwinder(this->modules), fp_unwinder(&this->cfi_unwinder) {}

  UnwinderData(const UnwinderData&) = delete;
  UnwinderData(UnwinderData&&) = delete;
  UnwinderData& operator=(const UnwinderData&) = delete;
  UnwinderData& operator=(UnwinderData&&) = delete;

  unwinder::FuchsiaMemory memory;
  std::vector<unwinder::Module> modules;

  // As each process has its own memory, we have an unwinder per process
  unwinder::CfiUnwinder cfi_unwinder;
  // mutable, because stepping the unwinder causes state to change
  mutable unwinder::FramePointerUnwinder fp_unwinder;
};

struct ProcessTarget {
  ProcessTarget(zx::process process, zx_koid_t pid, std::vector<ThreadTarget> threads)
      : handle(std::move(process)),
        pid(pid),
        threads(std::move(threads)),
        unwinder_data(std::make_unique<UnwinderData>(handle.borrow())) {}

  zx::process handle;
  zx_koid_t pid;
  std::vector<ThreadTarget> threads;
  std::unique_ptr<UnwinderData> unwinder_data;
};

struct JobTarget {
  explicit JobTarget(zx_koid_t job_id) : job_id(job_id) {}
  JobTarget(zx_koid_t job_id, std::vector<ProcessTarget> processes,
            std::vector<JobTarget> child_jobs)
      : job_id(job_id), processes(std::move(processes)), child_jobs(std::move(child_jobs)) {}

  JobTarget(const JobTarget&) = delete;
  JobTarget(JobTarget&&) = default;

  zx_koid_t job_id;
  std::vector<ProcessTarget> processes;
  std::vector<JobTarget> child_jobs;

  // Do a depth first search to call f on each process in the modeled job tree.
  zx::result<> ForEachProcess(const fit::function<zx::result<>(ProcessTarget& target)>& f);
};

// Given a process, create a process target containing it and all its threads
zx::result<profiler::ProcessTarget> MakeProcessTarget(zx::process process);

// Given a job, create a job target containing it, its processes, their threads, and its child jobs.
zx::result<profiler::JobTarget> MakeJobTarget(zx::job job);

}  // namespace profiler
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_TARGETS_H_
