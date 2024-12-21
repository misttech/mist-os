// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_SYSCALLS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_SYSCALLS_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/user_address.h>

#include <asm/stat.h>

namespace starnix {

using starnix_uapi::Errno;

class CurrentTask;

fit::result<Errno> sys_access(const CurrentTask& current_task, starnix_uapi::UserCString user_path,
                              starnix_uapi::FileMode mode);

fit::result<Errno, uint32_t> sys_alarm(const CurrentTask& current_task, uint32_t duration);

fit::result<Errno> sys_arch_prctl(CurrentTask& current_task, uint32_t code,
                                  starnix_uapi::UserAddress addr);

fit::result<Errno> sys_chmod(const CurrentTask& current_task, starnix_uapi::UserCString user_path,
                             starnix_uapi::FileMode mode);

fit::result<Errno> sys_chown(const CurrentTask& current_task, starnix_uapi::UserCString user_path,
                             uid_t owner, gid_t group);

/// The parameter order for `clone` varies by architecture.
fit::result<Errno, pid_t> sys_clone(CurrentTask& current_task, uint64_t flags,
                                    starnix_uapi::UserAddress user_stack,
                                    starnix_uapi::UserRef<pid_t> user_parent_tid,
                                    starnix_uapi::UserRef<pid_t> user_child_tid,
                                    starnix_uapi::UserAddress user_tls);

fit::result<Errno, pid_t> sys_fork(CurrentTask& current_task);

// https://pubs.opengroup.org/onlinepubs/9699919799/functions/creat.html
fit::result<Errno, FdNumber> sys_creat(const CurrentTask& current_task,
                                       starnix_uapi::UserCString user_path,
                                       starnix_uapi::FileMode mode);

fit::result<Errno, FdNumber> sys_dup2(const CurrentTask& current_task, FdNumber oldfd,
                                      FdNumber newfd);

fit::result<Errno, pid_t> sys_getpgrp(const CurrentTask& current_task);

fit::result<Errno> sys_link(const CurrentTask& current_task,
                            starnix_uapi::UserCString old_user_path,
                            starnix_uapi::UserCString new_user_path);

fit::result<Errno, FdNumber> sys_open(const CurrentTask& current_task,
                                      starnix_uapi::UserCString user_path, uint32_t flags,
                                      starnix_uapi::FileMode mode);

fit::result<Errno, size_t> sys_readlink(const CurrentTask& current_task,
                                        starnix_uapi::UserCString user_path,
                                        starnix_uapi::UserAddress buffer, size_t buffer_size);

fit::result<Errno> sys_rmdir(const CurrentTask& current_task, starnix_uapi::UserCString user_path);

fit::result<Errno> sys_stat(const CurrentTask& current_task, starnix_uapi::UserCString user_path,
                            starnix_uapi::UserRef<struct ::stat> buffer);

// https://man7.org/linux/man-pages/man2/symlink.2.html
fit::result<Errno> sys_symlink(const CurrentTask& current_task,
                               starnix_uapi::UserCString user_target,
                               starnix_uapi::UserCString user_path);

fit::result<Errno, __kernel_time_t> sys_time(const CurrentTask& current_task,
                                             starnix_uapi::UserRef<__kernel_time_t> time_addr);

fit::result<Errno> sys_unlink(const CurrentTask& current_task, starnix_uapi::UserCString user_path);

fit::result<Errno, pid_t> sys_vfork(CurrentTask& current_task);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_SYSCALLS_H_
