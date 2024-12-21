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

int64_t sys_a0084_rmdir(user_in_ptr<const char> pathname) {
  LTRACEF_LEVEL(2, "pathname=%p\n", pathname.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_rmdir, current_task,
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(pathname.get()))));
}

int64_t sys_a0086_link(user_in_ptr<const char> oldname, user_in_ptr<const char> newname) {
  LTRACEF_LEVEL(2, "oldname=%p, newname=%p\n", oldname.get(), newname.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_link, current_task,
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(oldname.get()))),
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(newname.get()))));
}

int64_t sys_a0087_unlink(user_in_ptr<const char> pathname) {
  LTRACEF_LEVEL(2, "pathname=%p\n", pathname.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_unlink, current_task,
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(pathname.get()))));
}

int64_t sys_a0088_symlink(user_in_ptr<const char> oldname, user_in_ptr<const char> newname) {
  LTRACEF_LEVEL(2, "oldname=%p, newname=%p\n", oldname.get(), newname.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_symlink, current_task,
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(oldname.get()))),
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(newname.get()))));
}

int64_t sys_a0258_mkdirat(int32_t dfd, user_in_ptr<const char> pathname, uint16_t mode) {
  LTRACEF_LEVEL(2, "dfd=%d, path=%p, mode=%x\n", dfd, pathname.get(), mode);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_mkdirat, current_task, starnix::FdNumber::from_raw(dfd),
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(pathname.get()))),
                         FileMode::from_bits(mode));
}

int64_t sys_a0263_unlinkat(int32_t dfd, user_in_ptr<const char> pathname, int32_t flag) {
  LTRACEF_LEVEL(2, "dfd=%d, pathname=%p, flag=0x%x\n", dfd, pathname.get(), flag);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_unlinkat, current_task, starnix::FdNumber::from_raw(dfd),
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(pathname.get()))),
                         flag);
}

int64_t sys_a0265_linkat(int32_t olddfd, user_in_ptr<const char> oldname, int32_t newdfd,
                         user_in_ptr<const char> newname, int32_t flags) {
  LTRACEF_LEVEL(2, ", olddfd=%d, oldname=%p, newdfd=%d, newname=%p, flag=0x%x\n", olddfd,
                oldname.get(), newdfd, newname.get(), flags);
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(starnix::sys_linkat, current_task, starnix::FdNumber::from_raw(olddfd),
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(oldname.get()))),
                         starnix::FdNumber::from_raw(newdfd),
                         starnix_uapi::UserCString::New(
                             UserAddress::from_ptr(reinterpret_cast<zx_vaddr_t>(newname.get()))),
                         flags);
}

int64_t sys_a0266_symlinkat(user_in_ptr<const char> oldname, int32_t newdfd,
                            user_in_ptr<const char> newname) {
  LTRACEF_LEVEL(2, "oldname=%p, newname=%p\n", oldname.get(), newname.get());
  auto& current_task = ThreadDispatcher::GetCurrent()->task()->into();
  return execute_syscall(
      starnix::sys_symlinkat, current_task,
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(oldname.get()))),
      starnix::FdNumber::from_raw(newdfd),
      starnix_uapi::UserCString::New(UserAddress::from_ptr((zx_vaddr_t)(newname.get()))));
}
