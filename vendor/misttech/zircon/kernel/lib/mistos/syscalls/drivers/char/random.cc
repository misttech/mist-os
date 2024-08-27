// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/syscalls/misc.h>
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

// clang format off
#include <lib/mistos/linux_uapi/typedefs.h>
// clang format on

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

using namespace starnix_uapi;
using namespace starnix;
using namespace starnix_syscalls;

long sys_getrandom(user_out_ptr<char> buffer, size_t count, unsigned int flags) {
  LTRACEF_LEVEL(2, "buffer %p count %zu flags 0x%x\n", buffer.get(), count, flags);
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getrandom(
      CurrentTask::From(TaskBuilder(ut->task()->task())),
      UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(buffer.get())), count, flags);
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}
