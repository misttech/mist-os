// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_SYSCALLS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_SYSCALLS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fd_numbers.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>

using namespace starnix_uapi;

namespace starnix {

class CurrentTask;

// sys_mmap takes a mutable reference to current_task because it may modify the IP register.
fit::result<Errno, UserAddress> sys_mmap(const CurrentTask& current_task, UserAddress addr,
                                         size_t length, uint32_t prot, uint32_t flags, FdNumber fd,
                                         uint64_t offset);

fit::result<Errno, UserAddress> do_mmap(const CurrentTask& current_task, UserAddress addr,
                                        size_t length, uint32_t prot, uint32_t flags, FdNumber fd,
                                        uint64_t offset);

fit::result<Errno> sys_munmap(const CurrentTask& current_task, UserAddress addr, size_t length);

fit::result<Errno> sys_mprotect(const CurrentTask& current_task, UserAddress addr, size_t length,
                                uint32_t prot);

fit::result<Errno, UserAddress> sys_mremap(const CurrentTask& current_task, UserAddress addr,
                                           size_t old_length, size_t new_length, uint32_t flags,
                                           UserAddress new_addr);

fit::result<Errno, UserAddress> sys_brk(const CurrentTask& current_task, UserAddress addr);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_SYSCALLS_H_
