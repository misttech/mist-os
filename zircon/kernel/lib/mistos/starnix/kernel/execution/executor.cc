// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/execution/executor.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/execution/shared.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/task_wrapper.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/zx/job.h>
#include <lib/mistos/zx/process.h>
#include <lib/mistos/zx/thread.h>
#include <trace.h>
#include <zircon/types.h>

#include <tuple>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<zx_status_t, TaskInfo> create_zircon_process(
    fbl::RefPtr<Kernel> kernel, std::optional<fbl::RefPtr<ThreadGroup>> parent, pid_t pid,
    fbl::RefPtr<ProcessGroup> process_group, const fbl::String& name) {
  LTRACE;
  auto result = create_shared(0, name);
  if (result.is_error()) {
    return result.take_error();
  }
  auto [process, root_vmar] = std::move(result.value());

  fbl::RefPtr<MemoryManager> memory_manager;
  zx_status_t status = MemoryManager::New(std::move(root_vmar), &memory_manager);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  fbl::RefPtr<ThreadGroup> thread_group;
  status = ThreadGroup::New(kernel, std::move(process), parent, pid, process_group, &thread_group);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  return fit::ok(TaskInfo{{}, thread_group, memory_manager});
}

fit::result<zx_status_t, std::pair<zx::process, zx::vmar>> create_shared(uint32_t options,
                                                                         const fbl::String& name) {
  LTRACE;
  zx::process process;
  zx::vmar vmar;
  zx_status_t status = zx::process::create(*zx::unowned_job{zx::job::default_job()}, name.data(),
                                           static_cast<uint32_t>(name.size()), 0, &process, &vmar);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  return fit::ok(std::make_pair(std::move(process), std::move(vmar)));
}

fit::result<zx_status_t, zx::thread> create_thread(const zx::process& parent,
                                                   const fbl::String& name) {
  LTRACE;
  zx::thread thread;
  zx_status_t status =
      zx::thread::create(parent, name.data(), static_cast<uint32_t>(name.size()), 0, &thread);

  if (status != ZX_OK) {
    return fit::error(status);
  }

  return fit::ok(std::move(thread));
}

// Runs the `current_task` to completion.
//
fit::result<zx_status_t, ExitStatus> run_task(CurrentTask& current_task) {
  LTRACE;
  // Start the process going.
  auto& thread = current_task->thread();
  if (thread.has_value()) {
    fbl::AllocChecker ac;
    // Set the task wrapper reference this will be used in syscalls
    // thread->get()->task_wrapper() =
    //    fbl::MakeRefCountedChecked<TaskWrapper>(&ac, current_task.task());
    // if (!ac.check()) {
    //  return fit::error(ZX_ERR_NO_MEMORY);
    //}

    auto& process = current_task->thread_group()->process();
    zx_status_t status =
        process.start(thread.value(), current_task.thread_state().registers.real_registers.rip,
                      current_task.thread_state().registers.real_registers.rsp, 0, 0);
    if (status != ZX_OK) {
      TRACEF("failed to start process %d\n", status);
      return fit::error(status);
    }

    status = process.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), nullptr);
    if (status != ZX_OK) {
      TRACEF("process.wait_one on process failed %d\n", status);
      return fit::error(status);
    }

    zx_info_process_t info;
    status = process.get_info(ZX_INFO_PROCESS, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      TRACEF("process.get_info on process failed %d\n", status);
      return fit::error(status);
    }

    return fit::ok(ExitStatus{});
  }
  return fit::error(ZX_ERR_INVALID_ARGS);
}

void execute_task_with_prerun_result(TaskBuilder task_builder, PreRun pre_run,
                                     TaskComplete task_complete) {
  LTRACE;
  execute_task(
      task_builder,
      [pre_run = std::move(pre_run)](CurrentTask& init_task) -> fit::result<Errno> {
        if (auto pre_run_result = pre_run(init_task); pre_run_result.is_error()) {
          return pre_run_result.take_error();
        }
        return fit::ok();
      },
      std::move(task_complete));
}

void execute_task(TaskBuilder task_builder, PreRun pre_run,
                                     TaskComplete task_complete/*,
                  std::optional<PtraceCoreState> ptrace_state*/) {
  LTRACE;
  // Set the process handle to the new task's process, so the new thread is spawned in that
  // process.

  // Hold a lock on the task's thread slot until we have a chance to initialize it.
  auto ref_task = task_builder.task();
  Guard<Mutex> lock(ref_task->thread_rw_lock());

#if 0
  // create a thread to complete task initialization
  dprintf(SPEW, "creating user-thread completion thread\n");

  struct ThreadArgs {
    TaskBuilder task_builder;
    PreRun pre_run;
  } args = {task_builder, std::move(pre_run)};

  Thread* t = Thread::Create(
      "user-thread",
      [](void* arg) -> int {
        ThreadArgs* args = static_cast<ThreadArgs*>(arg);

        auto current_task = CurrentTask::From(args->task_builder);
        auto pre_run_result = args->pre_run(current_task);
        if (pre_run_result.is_error()) {
          TRACEF("Pre run failed from %d. The task will not be run.",
                 pre_run_result.error_value().error_code());
        } else {
        }
        return 0;  // Return whatever integer value is appropriate
      },
      &args, DEFAULT_PRIORITY);
  t->Detach();
  t->Resume();

#endif

  auto current_task = CurrentTask::From(task_builder);
  auto pre_run_result = pre_run(current_task);
  if (pre_run_result.is_error()) {
    TRACEF("Pre run failed from %d. The task will not be run.",
           pre_run_result.error_value().error_code());
  } else {
    auto init_thread = create_thread(current_task->thread_group()->process(), "user-thread");
    if (!init_thread.is_error()) {
      // Spawn the process' thread.
      current_task->thread() = std::move(init_thread.value());
      auto exit_code = run_task(current_task);
      if (exit_code.is_error()) {
        TRACEF("Failed to run task %d\n", exit_code.error_value());
      } else {
        TRACEF("*** Exit status***\n");
      }
    }
  }
}

}  // namespace starnix
