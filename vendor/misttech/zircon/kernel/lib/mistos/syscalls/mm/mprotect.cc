// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/zircon/task_dispatcher.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include <cstdint>

#include "../priv.h"
#include "lib/mistos/starnix_uapi/user_address.h"

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

using namespace starnix_uapi;
using namespace starnix;
using namespace starnix_syscalls;

long sys_mprotect(unsigned long start, size_t len, unsigned long prot) {
  LTRACEF_LEVEL(2, "start=0x%lx len=%ld prot=0x%lx\n", start, len, prot);
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_mprotect(CurrentTask::From(TaskBuilder(ut->task()->task())),
                                  UserAddress(start), len, static_cast<uint32_t>(prot));
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(0).value();
}
