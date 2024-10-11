// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/signals/syscalls.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#include <linux/resource.h>

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

long sys_a0060_exit(int32_t error_code) {
  LTRACEF_LEVEL(2, "error_code=%d\n", error_code);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_exit, current_task, error_code);
}

long sys_a0061_wait4(int32_t upid, user_in_ptr<const int32_t> stat_addr, int32_t options,
                     user_out_ptr<void> ru) {
  LTRACEF_LEVEL(2, "upid=%d options=0x%x\n", upid, options);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_wait4, current_task, upid,
                         UserRef<int32_t>::From(UserAddress::from_ptr((zx_vaddr_t)stat_addr.get())),
                         static_cast<uint32_t>(options),
                         UserRef<struct rusage>::From(UserAddress::from_ptr(
                             (zx_vaddr_t)ru.reinterpret<struct rusage>().get())));
}

long sys_a0231_exit_group(int32_t error_code) {
  LTRACEF_LEVEL(2, "error_code=%d\n", error_code);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_exit_group, current_task, error_code);
}
