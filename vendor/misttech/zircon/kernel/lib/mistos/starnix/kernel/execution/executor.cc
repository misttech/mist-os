// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/execution/executor.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/execution/shared.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/zircon/task_dispatcher.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/mistos/zx/job.h>
#include <lib/mistos/zx/process.h>
#include <lib/mistos/zx/thread.h>
#include <lib/mistos/zx_syscalls/util.h>
#include <trace.h>
#include <zircon/types.h>

#include <tuple>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <object/dispatcher.h>
#include <object/job_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object_paged.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<Errno, TaskInfo> create_zircon_process(fbl::RefPtr<Kernel> kernel,
                                                   ktl::optional<fbl::RefPtr<ThreadGroup>> parent,
                                                   pid_t pid,
                                                   fbl::RefPtr<ProcessGroup> process_group,
                                                   const fbl::String& name) {
  LTRACE;
  auto shared_or_errorr =
      create_process(0, zx::unowned_job(process_group->job), name).map_error([](auto status) {
        return errno(from_status_like_fdio(status));
      });
  if (shared_or_errorr.is_error()) {
    return shared_or_errorr.take_error();
  }

  auto [process, root_vmar] = ktl::move(shared_or_errorr.value());

  auto mm_or_error = MemoryManager::New(ktl::move(root_vmar)).map_error([](auto status) {
    return errno(from_status_like_fdio(status));
  });
  if (mm_or_error.is_error()) {
    return mm_or_error.take_error();
  }

  auto thread_group = ThreadGroup::New(kernel, ktl::move(process), parent, pid, process_group);
  return fit::ok(TaskInfo{{}, thread_group, *mm_or_error});
}

fit::result<zx_status_t, ktl::pair<zx::process, zx::vmar>> create_shared(uint32_t options,
                                                                         const fbl::String& name) {
  LTRACE;
  zx::process process;
  zx::vmar vmar;
  zx_status_t status = zx::process::create(*zx::unowned_job{zx::job::default_job()}, name.data(),
                                           static_cast<uint32_t>(name.size()), 0, &process, &vmar);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  return fit::ok(ktl::pair(ktl::move(process), ktl::move(vmar)));
}

fit::result<zx_status_t, zx::job> create_job(uint32_t options) {
  LTRACE;
  zx::job job;
  zx_status_t status = zx::job::create(*zx::unowned_job{zx::job::default_job()}, options, &job);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  return fit::ok(ktl::move(job));
}

fit::result<zx_status_t, ktl::pair<zx::process, zx::vmar>> create_process(uint32_t options,
                                                                          zx::unowned_job job,
                                                                          const fbl::String& name) {
  LTRACE;
  zx::process process;
  zx::vmar vmar;
  zx_status_t status = zx::process::create(*job, name.data(), static_cast<uint32_t>(name.size()), 0,
                                           &process, &vmar);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  return fit::ok(ktl::pair(ktl::move(process), ktl::move(vmar)));
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

  return fit::ok(ktl::move(thread));
}

// Runs the `current_task` to completion.
//
fit::result<zx_status_t, ExitStatus> run_task(CurrentTask& current_task) {
  LTRACE;
  // Start the process going.
  auto& thread = *current_task->thread.Read();
  if (thread.has_value()) {
    auto& process = current_task->thread_group->process;

    ProcessDispatcher* up = nullptr;
    bool is_kernel_space = ThreadDispatcher::GetCurrent() == nullptr;
    if (!is_kernel_space) {
      up = ProcessDispatcher::GetCurrent();
    }

    TRACEF("Running task from %s space.\n", is_kernel_space ? "kernel" : "user");

    fbl::RefPtr<ThreadDispatcher> thread_dispatcher;
    zx_status_t status =
        handle_table(up).GetDispatcher(*up, thread.value().get(), &thread_dispatcher);
    if (status != ZX_OK) {
      TRACEF("failed to find current thread dispatcher %d\n", status);
      return fit::error(status);
    }
    ASSERT(thread_dispatcher);

    // Make task handle
    KernelHandle<TaskDispatcher> task_handle;
    zx_rights_t task_rights;
    status = TaskDispatcher::Create(current_task.task, &task_handle, &task_rights);
    if (status != ZX_OK) {
      TRACEF("failed to create task dispatcher %d\n", status);
      return fit::error(status);
    }
    thread_dispatcher->SetTask(task_handle.release());

    // When running from kernel (bootstrapping) we need to transfer handles
    if (is_kernel_space) {
      // Transfer handles to child process
      auto state = current_task->mm()->state.Write();

      // Transfer VMAR handle to child process
      zx_handle_t new_handle;
      TransferHandle<VmAddressRegionDispatcher>(process.get(), state->user_vmar.get(), &new_handle);
      state->user_vmar.reset(new_handle);

      // Transfer VMOs handles to child process
      for (auto& mapping : state->mappings.iter()) {
        ktl::visit(MappingBacking::overloaded{
                       [&process](MappingBackingVmo& backing) {
                         zx_handle_t new_handle;
                         // Transfer the Job handle
                         TransferHandle<VmObjectDispatcher>(
                             process.get(), backing.vmo()->as_ref().get(), &new_handle);
                         backing.vmo()->as_ref_mut().reset(new_handle);
                       },
                       [](PrivateAnonymous&) {},
                   },
                   mapping.second.backing().variant);
      }

      // Transfer Job handle to child process
      TransferHandle<JobDispatcher>(
          process.get(), current_task->thread_group->read().process_group->job.get(), &new_handle);
      current_task->thread_group->write().process_group->job.reset(new_handle);
    }

    status = process.start(thread.value(), current_task.thread_state.registers->rip,
                           current_task.thread_state.registers->rsp, {}, 0);
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
      [pre_run = ktl::move(pre_run)](CurrentTask& init_task) -> fit::result<Errno> {
        if (auto pre_run_result = pre_run(init_task); pre_run_result.is_error()) {
          return pre_run_result.take_error();
        }
        return fit::ok();
      },
      ktl::move(task_complete));
}

void execute_task(TaskBuilder task_builder, PreRun pre_run,
                                     TaskComplete task_complete/*,
                  std::optional<PtraceCoreState> ptrace_state*/) {
  LTRACE;
  // Set the process handle to the new task's process, so the new thread is spawned in that
  // process.

  auto weak_task = util::WeakPtr<Task>(task_builder.task.get());
  auto ref_task = weak_task.Lock();

  // Hold a lock on the task's thread slot until we have a chance to initialize it.
  // auto task_thread_guard = ref_task->thread.Write();

#if 0
  // create a thread to complete task initialization
  dprintf(SPEW, "creating user-thread completion thread\n");

  struct ThreadArgs {
    TaskBuilder task_builder;
    PreRun pre_run;
  } args = {task_builder, ktl::move(pre_run)};

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
    auto init_thread = create_thread(current_task->thread_group->process, "user-thread");
    if (!init_thread.is_error()) {
      // Spawn the process' thread.
      *ref_task->thread.Write() = ktl::move(init_thread.value());
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
