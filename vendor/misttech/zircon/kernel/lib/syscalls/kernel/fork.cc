// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

using starnix_uapi::UserAddress;

long sys_a0056_clone(uint64_t clone_flags, uint64_t newsp, user_out_ptr<int32_t> parent_tidptr,
                     user_out_ptr<int32_t> child_tidptr, uint64_t tls) {
  LTRACEF_LEVEL(2, "clone_flags=0x%lx, newsp=0x%lx, parent_tidptr=%p, child_tidptr=%p, tls=%lu\n",
                clone_flags, newsp, parent_tidptr.get(), child_tidptr.get(), tls);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_clone, current_task, clone_flags, UserAddress::const_from(newsp),
      UserRef<pid_t>::From(
          UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(parent_tidptr.get()))),
      UserRef<pid_t>::From(UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(child_tidptr.get()))),
      UserAddress::from_ptr(tls));
}

long sys_a0057_fork() {
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_fork, current_task);
}

long sys_a0058_vfork() {
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_vfork, current_task);
}
