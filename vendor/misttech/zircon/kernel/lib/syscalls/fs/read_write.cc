// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/syscalls.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

int64_t sys_a0000_read(unsigned int fd, user_out_ptr<char> buf, size_t count) {
  LTRACEF_LEVEL(2, "fd=%d count=%zu\n", fd, count);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_read, current_task,
                         starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)),
                         starnix_uapi::UserAddress::from_ptr((zx_vaddr_t)(buf.get())), count);
}

int64_t sys_a0001_write(unsigned int fd, user_in_ptr<const char> buf, size_t count) {
  LTRACEF_LEVEL(2, "fd=%d count=%zu\n", fd, count);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_write, current_task,
                         starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)),
                         starnix_uapi::UserAddress::from_ptr((zx_vaddr_t)(buf.get())), count);
}

int64_t sys_a0002_open(user_in_ptr<const char> filename, int flags, umode_t mode) {
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_open, current_task,
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)filename.get())), flags,
      FileMode(mode));
}

int64_t sys_a0003_close(unsigned int fd) {
  LTRACEF_LEVEL(2, "fd=%d\n", fd);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_close, current_task,
                         starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)));
}

int64_t sys_a0008_lseek(uint32_t fd, uint64_t offset, uint32_t whence) {
  LTRACEF_LEVEL(2, "fd=%d\n", fd);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_lseek, current_task,
                         starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)), offset, whence);
}

int64_t sys_a0017_pread64(unsigned int fd, user_out_ptr<char> buf, size_t count, int64_t pos) {
  LTRACEF_LEVEL(2, "fd=%d count=%zu pos=%ld\n", fd, count, pos);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_pread64, current_task,
                         starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)),
                         starnix_uapi::UserAddress::from_ptr((zx_vaddr_t)(buf.get())), count, pos);
}

int64_t sys_a0020_writev(unsigned long fd, user_in_ptr<const void> vec, unsigned long vlen) {
  LTRACEF_LEVEL(2, "fd=%lu count=%lu\n", fd, vlen);

  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_writev, current_task,
                         starnix::FdNumber::from_raw(static_cast<uint32_t>(fd)),
                         starnix_uapi::UserAddress::from_ptr((zx_vaddr_t)(vec.get())),
                         starnix_uapi::UserValue<uint32_t>(static_cast<uint32_t>(vlen)));
}
