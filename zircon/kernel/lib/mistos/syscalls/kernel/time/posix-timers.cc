// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/syscalls/time.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/zircon/task_dispatcher.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>

#include "../../priv.h"

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

using namespace starnix_uapi;
using namespace starnix;
using namespace starnix_syscalls;

long sys_clock_gettime(const clockid_t which_clock, user_out_ptr<struct timespec> tp) {
  LTRACEF_LEVEL(2, "which_clock %d buffer %p\n", which_clock, tp.get());
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret =
      sys_clock_gettime(CurrentTask::From(TaskBuilder(ut->task()->task())), which_clock,
                        UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(tp.get())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(0).value();
}
