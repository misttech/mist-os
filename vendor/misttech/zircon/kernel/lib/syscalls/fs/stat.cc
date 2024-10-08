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

long sys_a0004_stat(user_in_ptr<const char> filename, user_out_ptr<void> buf) {
  LTRACEF_LEVEL(2, "path=%p buf=%p\n", filename.get(), buf.get());
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_stat, current_task,
      starnix_uapi::UserCString::from_ptr((zx_vaddr_t)(filename.get())),
      starnix_uapi::UserRef<struct ::stat>::New(starnix_uapi::UserAddress::from_ptr(
          (zx_vaddr_t)(buf.reinterpret<struct ::stat>().get()))));
}

long sys_a0005_fstat(unsigned int fd, user_out_ptr<void> statbuf) {
  LTRACEF_LEVEL(2, "fd=%d buf=%p \n", fd, statbuf.get());
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_fstat, current_task, starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)),
      starnix_uapi::UserRef<struct ::stat>::New(starnix_uapi::UserAddress::from_ptr(
          (zx_vaddr_t)(statbuf.reinterpret<struct ::stat>().get()))));
}

long sys_a0332_statx(int dfd, user_in_ptr<const char> path, unsigned flags, unsigned mask,
                     user_out_ptr<void> buffer) {
  LTRACEF_LEVEL(2, "dfd=%d path=%p flags=%x mask=%x buf=%p \n", dfd, path.get(), flags, mask,
                buffer.get());
  return -EINVAL;
}
