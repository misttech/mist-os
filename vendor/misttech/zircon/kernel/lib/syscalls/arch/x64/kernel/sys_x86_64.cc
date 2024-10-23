// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <trace.h>

#include "../../../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

long sys_a0009_mmap(unsigned long addr, unsigned long len, unsigned long prot, unsigned long flags,
                    unsigned long fd, unsigned long off) {
  LTRACEF_LEVEL(2, "addr=0x%lx len=%lu prot=0x%lx flags=0x%lx fd=%ld off=%lu\n", addr, len, prot,
                flags, fd, off);

  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_mmap, current_task, UserAddress(addr), len,
                         static_cast<uint32_t>(prot), static_cast<uint32_t>(flags),
                         starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)), off);
}

long sys_a0011_munmap(unsigned long addr, unsigned long len) {
  LTRACEF_LEVEL(2, "addr=0x%lx len=%lu\n", addr, len);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_munmap, current_task, UserAddress(addr), len);
}
