// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_TARGETS_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_TARGETS_H_

#include <lib/fit/function.h>
#include <lib/stdcompat/span.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/thread.h>
#include <zircon/system/ulib/elf-search/include/elf-search.h>
#include <zircon/types.h>

#include <memory>
#include <optional>
#include <unordered_map>
#include <vector>

#include <src/lib/unwinder/cfi_unwinder.h>
#include <src/lib/unwinder/fp_unwinder.h>
#include <src/lib/unwinder/fuchsia.h>
#include <src/lib/unwinder/unwind.h>

#include "symbolization_context.h"

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
  ProcessTarget(zx::process process, zx_koid_t pid,
                std::unordered_map<zx_koid_t, ThreadTarget> threads)
      : handle(std::move(process)),
        pid(pid),
        threads(std::move(threads)),
        unwinder_data(std::make_unique<UnwinderData>(handle.borrow())) {}

  zx::process handle;
  zx_koid_t pid;
  std::unordered_map<zx_koid_t, ThreadTarget> threads;
  std::unique_ptr<UnwinderData> unwinder_data;
};

struct JobTarget {
  explicit JobTarget(zx::job job, zx_koid_t job_id, cpp20::span<const zx_koid_t> ancestry)
      : job(std::move(job)), job_id(job_id), ancestry(ancestry.begin(), ancestry.end()) {}
  JobTarget(zx::job job, zx_koid_t job_id, std::unordered_map<zx_koid_t, ProcessTarget> processes,
            std::unordered_map<zx_koid_t, JobTarget> child_jobs,
            cpp20::span<const zx_koid_t> ancestry)
      : job(std::move(job)),
        job_id(job_id),
        processes(std::move(processes)),
        child_jobs(std::move(child_jobs)),
        ancestry(ancestry.begin(), ancestry.end()) {}

  JobTarget(const JobTarget&) = delete;
  JobTarget(JobTarget&&) = default;

  zx::job job;
  zx_koid_t job_id;
  std::unordered_map<zx_koid_t, ProcessTarget> processes;
  std::unordered_map<zx_koid_t, JobTarget> child_jobs;

  // The list of ancestor jobs encountered while traversing starting at root job to this job.
  // Contains the root job, but does not contain this job.
  std::vector<zx_koid_t> ancestry;

  // Do a depth first search to call f on each process in the modeled job tree.
  zx::result<> ForEachProcess(
      const fit::function<zx::result<>(cpp20::span<const zx_koid_t> job_path,
                                       const ProcessTarget& target)>& f) const;
  // Do a depth first search to call f on each child job in the modeled job tree.
  zx::result<> ForEachJob(const fit::function<zx::result<>(const JobTarget& target)>& f) const;

  // Add `job` into the job tree as a child to the nested jobs specified by `ancestry`.
  //
  // Returns zx::ok if the job was successfully added,
  // ZX_ERR_NOT_FOUND if there is no matching ancestry
  // ZX_ERR_ALREADY_EXISTS if there is already an existing job at the location with the same job_id
  //
  // Note: `ancestry` is the jobs that `job` will be placed under and does not include the job_id of
  // `job` itself.
  zx::result<> AddJob(cpp20::span<const zx_koid_t> ancestry, JobTarget&& job);

  // Add `process` into the job tree as a child to the job specified by `job_path`
  //
  // Returns zx::ok if the process was successfully added,
  // ZX_ERR_NOT_FOUND if there is no matching job_path
  // ZX_ERR_ALREADY_EXISTS if there is already an existing process at the location with the same pid
  zx::result<> AddProcess(cpp20::span<const zx_koid_t> job_path, ProcessTarget&& process);

  // Add `thread` into the job tree as a child to process in job specified by `pid` and `job_path`.
  //
  // Returns zx::ok if the thread was successfully added,
  // ZX_ERR_NOT_FOUND if there is no matching job_path
  // ZX_ERR_ALREADY_EXISTS if there is already an existing thread at the location with the same tid
  zx::result<> AddThread(cpp20::span<const zx_koid_t> job_path, zx_koid_t pid,
                         ThreadTarget&& thread);
  zx::result<> RemoveThread(cpp20::span<const zx_koid_t> job_path, zx_koid_t pid, zx_koid_t tid);
};

class TargetTree {
 public:
  TargetTree() = default;
  TargetTree(const TargetTree&) = delete;
  TargetTree& operator=(const TargetTree&) = delete;

  TargetTree(TargetTree&&) = default;
  TargetTree& operator=(TargetTree&&) = default;

  // Add `job` into top level set of jobs in the tree.
  //
  // Returns zx::ok if the job was successfully added,
  // ZX_ERR_ALREADY_EXISTS if there is already an existing job with the same job_id
  zx::result<> AddJob(JobTarget&& job);

  // Add `thread` into the tree as a child to the process specified by `pid` with no parent job
  //
  // Returns zx::ok if the thread was successfully added,
  // ZX_ERR_NOT_FOUND if there is no process matching `pid`
  // ZX_ERR_ALREADY_EXISTS if there is already an existing thread at the location
  zx::result<> AddThread(zx_koid_t pid, ThreadTarget&& thread);
  zx::result<> RemoveThread(zx_koid_t pid, zx_koid_t tid);

  // Add `process` into the job tree as a process with no parent job
  //
  // Returns zx::ok if the process was successfully added,
  // ZX_ERR_ALREADY_EXISTS if there is already an existing process with the same pid
  zx::result<> AddProcess(ProcessTarget&& process);

  // Add `job` into the job tree as a child to the job specified by `ancestry`.
  //
  // Returns zx::ok if the job was successfully added,
  // ZX_ERR_NOT_FOUND if there is no matching ancestry
  // ZX_ERR_ALREADY_EXISTS if there is already an existing job at the location with the same job_id
  //
  // Note: `ancestry` is the jobs that `job` will be placed under and does not include the job_id of
  // `job` itself.
  zx::result<> AddJob(cpp20::span<const zx_koid_t> ancestry, JobTarget&& job);

  zx::result<> RemoveThread(cpp20::span<const zx_koid_t> job_path, zx_koid_t pid, zx_koid_t tid);

  // Add `thread` into the job tree as a child to the job specified by `job_path` in the process
  // `pid`
  //
  // Returns zx::ok if the thread was successfully added,
  // ZX_ERR_NOT_FOUND if there is no matching job_path
  // ZX_ERR_ALREADY_EXISTS if there is already an existing thread at the location with the same tid
  zx::result<> AddThread(cpp20::span<const zx_koid_t> job_path, zx_koid_t pid,
                         ThreadTarget&& thread);

  // Add `process` into the job tree as a child in the job by `job_path`.
  //
  // Returns zx::ok if the process was successfully added,
  // ZX_ERR_NOT_FOUND if there is no matching job_path
  // ZX_ERR_ALREADY_EXISTS if there is already an existing thread at the location with the same tid
  zx::result<> AddProcess(cpp20::span<const zx_koid_t> job_path, ProcessTarget&& process);
  void Clear();

  // Call `f` on each Job in the TargetTree. The order each job is visited is unspecified. If `f`
  // returns an error, the function will short circuit and immediately return the error code without
  // visiting any remaining jobs.
  zx::result<> ForEachJob(const fit::function<zx::result<>(const JobTarget& target)>& f);

  // Call `f` on each top level unparented process as well as every process in each added job. The
  // order each process is visited is unspecified. If `f` returns an error, the function will short
  // circuit and immediately return the error code without visiting any remaining processes.
  zx::result<> ForEachProcess(
      const fit::function<zx::result<>(cpp20::span<const zx_koid_t> job_path,
                                       const ProcessTarget& target)>& f);

  // Given a job, create a job target containing it, its processes, their threads, and its child
  // jobs. Additionally, the created job will be given the ancestry specified by `ancestry`.
  zx::result<profiler::JobTarget> MakeJobTarget(zx::job job, cpp20::span<const zx_koid_t> ancestry);

  // Given a job, create a job target containing it, its processes, their threads, and its child
  // jobs. The resulting job will have no parent or ancestors.
  zx::result<profiler::JobTarget> MakeJobTarget(zx::job job);

  // Given a process, create a process target containing it and all its threads
  zx::result<profiler::ProcessTarget> MakeProcessTarget(zx::process process);

  zx::result<std::vector<Module>> GetProcessModules(const zx::process&);

 private:
  std::unordered_map<zx_koid_t, JobTarget> jobs_;
  std::unordered_map<zx_koid_t, ProcessTarget> processes_;

  elf_search::Searcher searcher_;
};

}  // namespace profiler
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_TARGETS_H_
