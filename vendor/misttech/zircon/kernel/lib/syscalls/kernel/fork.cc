// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/zircon/task_dispatcher.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

using namespace starnix_uapi;
using namespace starnix;
using namespace starnix_syscalls;

long sys_clone(uint64_t clone_flags, uint64_t newsp, user_out_ptr<int32_t> parent_tidptr,
               user_out_ptr<int32_t> child_tidptr, uint64_t tls) {
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret =
      sys_clone(CurrentTask::From(TaskBuilder(ut->task()->task())), clone_flags, newsp,
                UserRef<pid_t>(UserAddress()), UserRef<pid_t>(UserAddress()), tls);
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_vfork() { return -ENOSYS; }

long sys_fork() {
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_fork(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}
