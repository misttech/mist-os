// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "targets.h"

#include "zircon/system/ulib/elf-search/include/elf-search.h"

zx::result<> profiler::JobTarget::ForEachProcess(
    const fit::function<zx::result<>(ProcessTarget& target)>& f) {
  for (ProcessTarget& p : processes) {
    zx::result<> res = f(p);
    if (res.is_error()) {
      return res;
    }
  }
  for (JobTarget& j : child_jobs) {
    zx::result<> res = j.ForEachProcess(f);
    if (res.is_error()) {
      return res;
    }
  }
  return zx::ok();
}

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

zx::result<profiler::ProcessTarget> profiler::MakeProcessTarget(zx::process process) {
  zx::result<std::vector<zx_koid_t>> children = GetChildrenTids(process);
  if (children.is_error()) {
    return children.take_error();
  }
  zx_info_handle_basic_t handle_info;
  zx_status_t res =
      process.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info), nullptr, nullptr);
  if (res != ZX_OK) {
    return zx::error(res);
  }
  std::vector<profiler::ThreadTarget> threads;
  for (auto child_tid : *children) {
    zx::thread child_thread;
    zx_status_t res = process.get_child(child_tid, ZX_DEFAULT_THREAD_RIGHTS, &child_thread);
    if (res != ZX_OK) {
      FX_PLOGS(ERROR, res) << "Failed to get handle for child (tid: " << child_tid << ")";
      continue;
    }
    threads.push_back(profiler::ThreadTarget{std::move(child_thread), child_tid});
  }
  profiler::ProcessTarget process_target{std::move(process), handle_info.koid, std::move(threads)};

  elf_search::ForEachModule(*zx::unowned_process{process_target.handle},
                            [&process_target](const elf_search::ModuleInfo& info) {
                              process_target.unwinder_data->modules.emplace_back(
                                  info.vaddr, &process_target.unwinder_data->memory,
                                  unwinder::Module::AddressMode::kProcess);
                            });
  return zx::ok(std::move(process_target));
}

zx::result<profiler::JobTarget> profiler::MakeJobTarget(zx::job job) {
  size_t num_child_jobs;
  zx_status_t status = job.get_info(ZX_INFO_JOB_CHILDREN, nullptr, 0, nullptr, &num_child_jobs);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to query number of job children";
    return zx::error(status);
  }
  std::vector<profiler::JobTarget> child_job_targets;
  if (num_child_jobs > 0) {
    zx_koid_t child_jobs[num_child_jobs];
    status = job.get_info(ZX_INFO_JOB_CHILDREN, child_jobs, sizeof(child_jobs), nullptr, nullptr);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "failed to get job children";
      return zx::error(status);
    }

    for (zx_koid_t child_koid : child_jobs) {
      zx::job child_job;
      zx_status_t res = job.get_child(child_koid, ZX_DEFAULT_JOB_RIGHTS, &child_job);
      if (status != ZX_OK) {
        FX_PLOGS(ERROR, res) << "failed to get job: " << child_koid;
        return zx::error(status);
      }
      zx::result<profiler::JobTarget> child_job_target = MakeJobTarget(std::move(child_job));
      if (child_job_target.is_error()) {
        FX_PLOGS(ERROR, child_job_target.status_value()) << "failed to make job_target";
        return zx::error(status);
      }
      child_job_targets.push_back(std::move(*child_job_target));
    }
  }

  size_t num_processes;
  status = job.get_info(ZX_INFO_JOB_PROCESSES, nullptr, 0, nullptr, &num_processes);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to query number of job processes";
    return zx::error(status);
  }
  std::vector<profiler::ProcessTarget> process_targets;
  if (num_processes > 0) {
    zx_koid_t processes[num_processes];
    status = job.get_info(ZX_INFO_JOB_PROCESSES, processes, sizeof(processes), nullptr, nullptr);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "failed to get job processes";
      return zx::error(status);
    }

    for (zx_koid_t process_koid : processes) {
      zx::process process;
      zx_status_t res = job.get_child(process_koid, ZX_DEFAULT_PROCESS_RIGHTS, &process);
      if (status != ZX_OK) {
        FX_PLOGS(ERROR, res) << "failed to get process: " << process_koid;
        return zx::error(status);
      }
      zx::result<profiler::ProcessTarget> process_target = MakeProcessTarget(std::move(process));
      if (process_target.is_error()) {
        FX_PLOGS(ERROR, process_target.status_value()) << "failed to make process_target";
        return zx::error(status);
      }
      process_targets.push_back(std::move(*process_target));
    }
  }

  zx_info_handle_basic_t info;
  zx_status_t info_res = job.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (info_res != ZX_OK) {
    FX_PLOGS(ERROR, info_res) << "failed to make process_target";
    return zx::error(status);
  }
  return zx::ok(
      profiler::JobTarget{info.koid, std::move(process_targets), std::move(child_job_targets)});
}
