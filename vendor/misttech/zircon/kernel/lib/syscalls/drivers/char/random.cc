// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/syscalls/misc.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/zx_cprng_draw.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>

#include "../../priv.h"

#include <linux/random.h>

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

long sys_a0318_getrandom(user_out_ptr<char> buffer, size_t count, unsigned int flags) {
  LTRACEF_LEVEL(2, "buffer %p count %zu flags 0x%x\n", buffer.get(), count, flags);

#if 0
  if ((flags & !(GRND_RANDOM | GRND_NONBLOCK)) != 0) {
    return -EINVAL;
  }
  _zx_cprng_draw(buffer.reinterpret<void>(), count);
  return 0;
#else
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_getrandom, current_task,
                         starnix_uapi::UserAddress::from_ptr((zx_vaddr_t)(buffer.get())), count,
                         flags);
#endif
}
