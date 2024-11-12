// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

using starnix_uapi::UserAddress;
using starnix_uapi::UserCString;
using starnix_uapi::UserRef;

long sys_a0059_execve(user_in_ptr<const char> filename, user_in_ptr<const char> argv,
                      user_in_ptr<const char> envp) {
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  auto user_argv = UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(argv.get())));
  auto user_envp = UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(envp.get())));
  return execute_syscall(starnix::sys_execve, current_task,
                         UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(filename.get()))),
                         UserRef<UserCString>::From(*user_argv),
                         UserRef<UserCString>::From(*user_envp));
}

long sys_a0322_execveat(int32_t fd, user_in_ptr<const char> filename, user_in_ptr<const char> argv,
                        user_in_ptr<const char> envp, int32_t flags) {
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  auto user_filename = UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(filename.get())));
  auto user_argv = UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(argv.get())));
  auto user_envp = UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(envp.get())));
  return execute_syscall(starnix::sys_execveat, current_task, starnix::FdNumber::from_raw(fd),
                         user_filename, UserRef<UserCString>::From(*user_argv),
                         UserRef<UserCString>::From(*user_envp), flags);
}
