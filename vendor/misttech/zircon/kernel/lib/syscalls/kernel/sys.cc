// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/zircon/task_dispatcher.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

using namespace starnix_uapi;
using namespace starnix;
using namespace starnix_syscalls;

long sys_getpid() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getpid(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_gettid() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_gettid(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_getppid() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getppid(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_getpgid(pid_t pid) {
  LTRACEF_LEVEL(2, "pid %d\n", pid);
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getpgid(CurrentTask::From(TaskBuilder(ut->task()->task())), pid);
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_getpgrp() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getpgrp(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_getuid() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getuid(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_getgid() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getgid(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_geteuid() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_geteuid(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_getegid() {
  LTRACE;
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_getegid(CurrentTask::From(TaskBuilder(ut->task()->task())));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_prctl(int option, unsigned long arg2, unsigned long arg3, unsigned long arg4,
               unsigned long arg5) {
  LTRACEF_LEVEL(2, "option %d arg2 0x%lx arg3 0x%lx arg4 0x%lx arg5 0x%lx\n", option, arg2, arg3,
                arg4, arg5);

  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret =
      sys_prctl(CurrentTask::From(TaskBuilder(ut->task()->task())), option, arg2, arg3, arg4, arg5);
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return syscall_ret->value();
}
