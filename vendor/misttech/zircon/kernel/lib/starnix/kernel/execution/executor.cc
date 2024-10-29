// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/execution/executor.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/signals/types.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <ktl/tuple.h>
#include <object/dispatcher.h>
#include <object/job_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object_paged.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<Errno, TaskInfo> create_zircon_process(
    fbl::RefPtr<Kernel> kernel,
    ktl::optional<RwLock<ThreadGroupMutableState>::RwLockWriteGuard> parent, pid_t pid,
    fbl::RefPtr<ProcessGroup> process_group, fbl::RefPtr<SignalActions> signal_actions,
    const ktl::string_view& name) {
  LTRACE;
  auto process_dispatcher =
      create_process(GetRootJobDispatcher(), 0, name).map_error([](auto status) {
        return errno(from_status_like_fdio(status));
      }) _EP(process_dispatcher);

  auto& [process, root_vmar] = *process_dispatcher;

  auto mm = MemoryManager::New(ktl::move(root_vmar)).map_error([](auto status) {
    return errno(from_status_like_fdio(status));
  }) _EP(mm);

  auto thread_group = ThreadGroup::New(kernel, ktl::move(process), ktl::move(parent), pid,
                                       process_group, signal_actions);

  return fit::ok(TaskInfo{.thread = {},
                          .thread_group = ktl::move(thread_group),
                          .memory_manager = ktl::move(mm.value())});
}

fit::result<zx_status_t, ktl::pair<KernelHandle<ProcessDispatcher>, Vmar>> create_process(
    fbl::RefPtr<JobDispatcher> parent, uint32_t options, const ktl::string_view& name) {
  KernelHandle<ProcessDispatcher> process_handle;
  KernelHandle<VmAddressRegionDispatcher> vmar_handle;
  zx_rights_t process_rights, vmar_rights;
  zx_status_t result = ProcessDispatcher::Create(parent, name, 0, &process_handle, &process_rights,
                                                 &vmar_handle, &vmar_rights);

  if (result != ZX_OK) {
    return fit::error(result);
  }

  KTRACE_KERNEL_OBJECT("kernel:meta", process_handle.dispatcher()->get_koid(), ZX_OBJ_TYPE_PROCESS,
                       name.data());

  return fit::ok(ktl::pair(ktl::move(process_handle),
                           Vmar{Handle::Make(ktl::move(vmar_handle), vmar_rights)}));
}

fit::result<zx_status_t, KernelHandle<ThreadDispatcher>> create_thread(
    fbl::RefPtr<ProcessDispatcher> parent, const ktl::string_view& name) {
  KernelHandle<ThreadDispatcher> handle;
  zx_rights_t thread_rights;
  zx_status_t status = ThreadDispatcher::Create(parent, 0, name, &handle, &thread_rights);
  if (status != ZX_OK) {
    return fit::error(status);
  }
  status = handle.dispatcher()->Initialize();
  if (status != ZX_OK) {
    return fit::error(status);
  }
  return fit::ok(ktl::move(handle));
}

fit::result<zx_status_t> run_task(CurrentTask current_task) {
  auto thread_lock = current_task->thread_.Read();
  if (thread_lock->has_value()) {
    auto thread = thread_lock->value();
    auto task = current_task.weak_task().Lock();
    auto thread_state = current_task.thread_state();
    fbl::AllocChecker ac;
    thread->SetTask(fbl::MakeRefCountedChecked<TaskWrapper>(&ac, ktl::move(current_task)));
    if (!ac.check()) {
      return fit::error(ZX_ERR_NO_MEMORY);
    }
    ZX_ASSERT(thread->AddObserver(task->observer(), task.get(), ZX_THREAD_TERMINATED) == ZX_OK);

    auto status = thread->Start(ThreadDispatcher::EntryState{.pc = thread_state.registers->rip,
                                                             .sp = thread_state.registers->rsp,
                                                             .arg1 = {},
                                                             .arg2 = 0},
                                /* ensure_initial_thread= */ false);
    if (status != ZX_OK) {
      TRACEF("failed to start process %d\n", status);
      return fit::error(status);
    }
  }
  return fit::ok();
}

}  // namespace starnix
