// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
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
class CurrentTask;
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

#if 0
using PreRun = std::function<fit::result<Errno>(CurrentTask& init_task)>;
using TaskComplete = std::function<void()>;

void execute_task_with_prerun_result(TaskBuilder task_builder, PreRun pre_run,
                                     TaskComplete task_complete);

void execute_task(TaskBuilder task_builder, PreRun pre_run,
                                     TaskComplete task_complete/*,
                  std::optional<PtraceCoreState> ptrace_state*/);
#endif

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_
