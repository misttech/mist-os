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

int64_t sys_a0080_chdir(user_in_ptr<const char> filename) {
  LTRACEF_LEVEL(2, "filename=%p\n", filename.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_chdir, current_task,
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)filename.get())));
}

int64_t sys_a0090_chmod(user_in_ptr<const char> filename, uint16_t mode) {
  LTRACEF_LEVEL(2, "filename=%p\n", filename.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_chmod, current_task,
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)filename.get())),
      FileMode::from_bits(mode));
}

int64_t sys_a0091_fchmod(uint32_t fd, uint16_t mode) {
  LTRACEF_LEVEL(2, "fd=%u, mode=0x%x\n", fd, mode);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_fchmod, current_task, starnix::FdNumber::from_raw(fd),
                         FileMode::from_bits(mode));
}

int64_t sys_a0161_chroot(user_in_ptr<const char> filename) {
  LTRACEF_LEVEL(2, "filename=%p\n", filename.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_chroot, current_task,
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)filename.get())));
}

int64_t sys_a0257_openat(int32_t dfd, user_in_ptr<const char> filename, int32_t flags,
                         uint16_t mode) {
  LTRACEF_LEVEL(2, "dfd=%d path=%p flags=0x%x mode=0x%x\n", dfd, filename.get(), flags, mode);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_openat, current_task, starnix::FdNumber::from_raw(dfd),
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)filename.get())), flags,
      FileMode::from_bits(mode));
}

int64_t sys_a0268_fchmodat(int64_t dfd, user_in_ptr<const char> filename, uint16_t mode) {
  LTRACEF_LEVEL(2, "filename=%p\n", filename.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_fchmodat, current_task, starnix::FdNumber::from_raw(static_cast<int32_t>(dfd)),
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)filename.get())),
      FileMode::from_bits(mode));
}

int64_t sys_a0452_fchmodat2(int32_t dfd, user_in_ptr<const char> filename, uint16_t mode,
                            uint32_t flags) {
  LTRACEF_LEVEL(2, "dfd=%d path=%p mode=0x%x, flags=0x%x\n", dfd, filename.get(), mode, flags);
  return errno(ENOTSUP).return_value();
}
