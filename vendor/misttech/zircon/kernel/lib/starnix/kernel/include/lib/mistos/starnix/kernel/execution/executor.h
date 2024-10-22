// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/starnix_sync/locks.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>
#include <ktl/pair.h>
#include <object/handle.h>

class ThreadDispatcher;
class ProcessDispatcher;
class JobDispatcher;

namespace starnix {

class Kernel;
class MemoryManager;
class ThreadGroup;
class ProcessGroup;
struct Vmar;

/// Result returned when creating new Zircon threads and processes for tasks.
struct TaskInfo {
  // The thread that was created for the task.
  ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread;

  // The thread group that the task should be added to.
  fbl::RefPtr<ThreadGroup> thread_group;

  // The memory manager to use for the task.
  fbl::RefPtr<MemoryManager> memory_manager;
};

fit::result<Errno, TaskInfo> create_zircon_process(
    fbl::RefPtr<Kernel> kernel,
    ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard> parent,
    pid_t pid, fbl::RefPtr<ProcessGroup> process_group, const ktl::string_view& name);

fit::result<zx_status_t, ktl::pair<KernelHandle<ProcessDispatcher>, Vmar>> create_process(
    fbl::RefPtr<JobDispatcher> job, uint32_t options, const ktl::string_view& name);

fit::result<zx_status_t, KernelHandle<ThreadDispatcher>> create_thread(
    fbl::RefPtr<ProcessDispatcher> parent, const ktl::string_view& name);

fit::result<zx_status_t> run_task(const CurrentTask& current_task);

template <typename PreRunFn, typename TaskCompleteFn>
void execute_task(TaskBuilder task_builder, PreRunFn&& pre_run,
                                     TaskCompleteFn&& task_complete/*,
                  std::optional<PtraceCoreState> ptrace_state*/){
  auto weak_task = util::WeakPtr<Task>(task_builder.task().get());
  auto ref_task = weak_task.Lock();

  // Hold a lock on the task's thread slot until we have a chance to initialize it.
  auto task_thread_guard = ref_task->thread().Write();

  auto user_thread =
      create_thread(ref_task->thread_group()->process().dispatcher(), ref_task->command());
  if (user_thread.is_ok()) {
    *task_thread_guard = ktl::move(user_thread.value().release());
  }
  std::destroy_at(std::addressof(task_thread_guard));

  auto current_task = CurrentTask::From(ktl::move(task_builder));
  auto pre_run_result = pre_run(current_task);
  if (pre_run_result.is_error()) {
    TRACEF("Pre run failed from %d. The task will not be run.",
           pre_run_result.error_value().error_code());
  } else {
    // Spawn the process' thread.
    auto run_result = run_task(current_task);
    if (run_result.is_error()) {
      TRACEF("Failed to run task %d\n", run_result.error_value());
    }
    task_complete(run_result);
  }
}

template <typename PreRunFn, typename TaskCompleteFn>
void execute_task_with_prerun_result(TaskBuilder task_builder, PreRunFn&& pre_run,
                                     TaskCompleteFn&& task_complete) {
  // Start the process going.
  execute_task(
      ktl::move(task_builder),
      [pre_run = ktl::move(pre_run)](CurrentTask& init_task) -> fit::result<Errno> {
        _EP(pre_run(init_task));
        return fit::ok();
      },
      ktl::move(task_complete));
}

template <typename E, typename T>
starnix_syscalls::SyscallResult SyscallResultProxy(fit::result<E, T> result) {
  return starnix_syscalls::SyscallResult::From(result.value());
}

template <typename E>
starnix_syscalls::SyscallResult SyscallResultProxy(fit::result<E> result) {
  return starnix_syscalls::SyscallResult::From(starnix_syscalls::SUCCESS);
}

template <typename SyscallFn, typename... Args>
uint64_t execute_syscall(SyscallFn&& syscall, Args&&... args) {
  // profile_duration!("ExecuteSyscall");
  auto result = syscall(std::forward<Args>(args)...);
  if (result.is_error()) {
    return result.error_value().return_value();
  }
  return SyscallResultProxy(result).value();
}

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_
