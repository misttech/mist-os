// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/syscalls/misc.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#include <linux/utsname.h>

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(2)

long sys_a0039_getpid() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getpid, current_task);
}

long sys_a0063_uname(user_out_ptr<void> name) {
#if 1
  struct new_utsname result;
  strncpy(result.sysname, "Linux", sizeof(result.sysname) / sizeof(result.sysname[0]));

  if (auto status = name.reinterpret<struct new_utsname>().copy_to_user(result); status != ZX_OK) {
    return -EFAULT;
  }

  return 0;
#else
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_uname, current_task, name.reinterpret<struct new_utsname>());
#endif
}

long sys_a0102_getuid() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getuid, current_task);
}

long sys_a0104_getgid() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getgid, current_task);
}

long sys_a0107_geteuid() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_geteuid, current_task);
}

long sys_a0108_getegid() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getegid, current_task);
}

long sys_a0110_getppid() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getppid, current_task);
}

long sys_a0111_getpgrp() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getpgrp, current_task);
}

long sys_a0186_gettid() {
  LTRACE;
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_gettid, current_task);
}

long sys_a0121_getpgid(pid_t pid) {
  LTRACEF_LEVEL(2, "pid %d\n", pid);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getpgid, current_task, pid);
}

long sys_a0157_prctl(int option, unsigned long arg2, unsigned long arg3, unsigned long arg4,
                     unsigned long arg5) {
  LTRACEF_LEVEL(2, "option %d arg2 0x%lx arg3 0x%lx arg4 0x%lx arg5 0x%lx\n", option, arg2, arg3,
                arg4, arg5);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_prctl, current_task, option, arg2, arg3, arg4, arg5);
}
