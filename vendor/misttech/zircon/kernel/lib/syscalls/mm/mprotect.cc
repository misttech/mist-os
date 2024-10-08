// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

long sys_a0010_mprotect(uint64_t start, size_t len, uint64_t prot) {
  LTRACEF_LEVEL(2, "start=0x%lx len=%ld prot=0x%lx\n", start, len, prot);
  auto current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_mprotect, current_task, UserAddress::const_from(start), len,
                         static_cast<uint32_t>(prot));
}
