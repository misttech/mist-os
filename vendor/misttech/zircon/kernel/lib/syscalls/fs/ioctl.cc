// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/syscalls.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

using starnix_uapi::UserAddress;

long sys_a0016_ioctl(uint32_t fd, uint32_t cmd, uint64_t arg) {
  LTRACEF_LEVEL(2, "fd=%u, cmd=%u, arg=%lu\n", fd, cmd, arg);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_ioctl, current_task, starnix::FdNumber::from_raw(fd), cmd,
                         starnix_syscalls::SyscallArg::from_raw(arg));
}
