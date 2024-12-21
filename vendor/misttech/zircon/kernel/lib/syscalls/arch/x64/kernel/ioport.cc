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

#include "../../../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

using starnix_uapi::UserAddress;

long sys_a0173_ioperm(uint64_t from, uint64_t num, int32_t turn_on) {
  LTRACEF_LEVEL(2, "from=%lu, num=%lu, turn_on=%d\n", from, num, turn_on);
  //auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  /*return execute_syscall(starnix::sys_pipe2, current_task,
                         UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(filename.get())), size);
                         */
  return -1;
}
