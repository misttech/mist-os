// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/syscalls.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

int64_t sys_a0032_dup(uint32_t fildes) {
  LTRACEF_LEVEL(2, "fildes=%u\n", fildes);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_dup, current_task, starnix::FdNumber::from_raw(fildes));
}

int64_t sys_a0033_dup2(uint32_t oldfd, uint32_t newfd) {
  LTRACEF_LEVEL(2, "oldfd=%u, newfd=%u\n", oldfd, newfd);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_dup2, current_task, starnix::FdNumber::from_raw(oldfd),
                         starnix::FdNumber::from_raw(newfd));
}

int64_t sys_a0292_dup3(uint32_t oldfd, uint32_t newfd, int32_t flags) {
  LTRACEF_LEVEL(2, "oldfd=%u, newfd=%u, flags=0x%x\n", oldfd, newfd, flags);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_dup3, current_task, starnix::FdNumber::from_raw(oldfd),
                         starnix::FdNumber::from_raw(newfd), static_cast<uint32_t>(flags));
}
