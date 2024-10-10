// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/syscalls.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(2)

int64_t sys_a0258_mkdirat(int32_t dfd, user_in_ptr<const char> pathname, uint16_t mode) {
  LTRACEF_LEVEL(2, "dfd=%d path=%p mode=%x\n", dfd, pathname.get(), mode);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_mkdirat, current_task, starnix::FdNumber::from_raw(dfd),
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(pathname.get()))),
      FileMode::from_bits(mode));
}
