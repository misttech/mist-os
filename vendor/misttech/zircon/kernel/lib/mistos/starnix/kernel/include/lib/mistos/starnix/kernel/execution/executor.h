// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/execution/shared.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/zx/job.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <ktl/optional.h>

namespace starnix {

// TODO (Herrera) make parent  /*parent: Option<ThreadGroupWriteGuard<'_>>,*/
fit::result<Errno, TaskInfo> create_zircon_process(fbl::RefPtr<Kernel> kernel,
                                                   ktl::optional<fbl::RefPtr<ThreadGroup>> parent,
                                                   pid_t pid,
                                                   fbl::RefPtr<ProcessGroup> process_group,
                                                   const fbl::String& name);

/// NOTE: We keep the name from Rust/Starnix but it is not creating a shared process now.
/// Creates a process that shares half its address space with this process.
///
/// The created process will also share its handle table and futex context with `self`.
///
/// Returns the created process and a handle to the created process' restricted address space.
///
/// Wraps the
/// [zx_process_create_shared](https://fuchsia.dev/fuchsia-src/reference/syscalls/process_create_shared.md)
/// syscall.
fit::result<zx_status_t, ktl::pair<zx::process, zx::vmar>> create_shared(uint32_t options,
                                                                         const fbl::String& name);

fit::result<zx_status_t, zx::job> create_job(uint32_t options);

fit::result<zx_status_t, ktl::pair<zx::process, zx::vmar>> create_process(uint32_t options,
                                                                          zx::unowned_job job,
                                                                          const fbl::String& name);

using PreRun = std::function<fit::result<Errno>(CurrentTask& init_task)>;
using TaskComplete = std::function<void()>;

void execute_task_with_prerun_result(TaskBuilder task_builder, PreRun pre_run,
                                     TaskComplete task_complete);

void execute_task(TaskBuilder task_builder, PreRun pre_run,
                                     TaskComplete task_complete/*,
                  std::optional<PtraceCoreState> ptrace_state*/);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_EXECUTOR_H_
