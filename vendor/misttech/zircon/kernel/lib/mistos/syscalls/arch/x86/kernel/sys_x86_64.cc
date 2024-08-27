// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/fd_numbers.h>
#include <lib/mistos/starnix/kernel/zircon/task_dispatcher.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../../../priv.h"

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

using namespace starnix_uapi;
using namespace starnix;
using namespace starnix_syscalls;

long sys_mmap(unsigned long addr, unsigned long len, unsigned long prot, unsigned long flags,
              unsigned long fd, unsigned long off) {
  LTRACEF_LEVEL(2, "addr=0x%lx len=%lu prot=0x%lx flags=0x%lx fd=%ld off=%lu\n", addr, len, prot,
                flags, fd, off);
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_mmap(CurrentTask::From(TaskBuilder(ut->task()->task())), UserAddress(addr),
                              len, static_cast<uint32_t>(prot), static_cast<uint32_t>(flags),
                              FdNumber::from_raw(static_cast<uint32_t>(fd)), off);
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(*syscall_ret).value();
}

long sys_munmap(unsigned long addr, unsigned long len) {
  LTRACEF_LEVEL(2, "addr=0x%lx len=%lu\n", addr, len);
  auto ut = ThreadDispatcher::GetCurrent();
  auto syscall_ret = sys_munmap(CurrentTask::From(TaskBuilder(ut->task()->task())), addr, len);
  if (syscall_ret.is_error()) {
    return syscall_ret.error_value().return_value();
  }
  return SyscallResult::From(0).value();
}
