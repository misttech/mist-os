// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYSCALLS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYSCALLS_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/starnix_uapi/user_value.h>

#include <asm/stat.h>
#include <linux/openat2.h>

namespace starnix {

class CurrentTask;

fit::result<Errno, size_t> sys_read(const CurrentTask& current_task, FdNumber fd,
                                    starnix_uapi::UserAddress address, size_t length);

fit::result<Errno, size_t> sys_write(const CurrentTask& current_task, FdNumber fd,
                                     starnix_uapi::UserAddress address, size_t length);

fit::result<Errno> sys_close(const CurrentTask& current_task, FdNumber fd);

fit::result<Errno> sys_close_range(const CurrentTask& current_task, uint32_t first, uint32_t last,
                                   uint32_t flags);

fit::result<Errno, off_t> sys_lseek(const CurrentTask& current_task, FdNumber fd, off_t offset,
                                    uint32_t whence);

fit::result<Errno, starnix_syscalls::SyscallResult> sys_fcntl(const CurrentTask& current_task,
                                                              FdNumber fd, uint32_t cmd,
                                                              uint64_t arg);

fit::result<Errno, size_t> sys_pread64(const CurrentTask& current_task, FdNumber fd,
                                       starnix_uapi::UserAddress address, size_t length,
                                       off_t offset);

fit::result<Errno, size_t> sys_pwrite64(const CurrentTask& current_task, FdNumber fd,
                                        starnix_uapi::UserAddress address, size_t length,
                                        off_t offset);

fit::result<Errno, size_t> sys_readv(const CurrentTask& current_task, FdNumber fd,
                                     starnix_uapi::UserAddress iovec_addr,
                                     starnix_uapi::UserValue<uint32_t> iovec_count);

fit::result<Errno, size_t> sys_preadv(const CurrentTask& current_task, FdNumber fd,
                                      starnix_uapi::UserAddress iovec_addr,
                                      starnix_uapi::UserValue<uint32_t> iovec_count, off_t offset);

fit::result<Errno, size_t> sys_preadv2(const CurrentTask& current_task, FdNumber fd,
                                       starnix_uapi::UserAddress iovec_addr,
                                       starnix_uapi::UserValue<uint32_t> iovec_count, off_t offset,
                                       uint64_t arg, uint32_t flags);

fit::result<Errno, size_t> sys_writev(const CurrentTask& current_task, FdNumber fd,
                                      starnix_uapi::UserAddress iovec_addr,
                                      starnix_uapi::UserValue<uint32_t> iovec_count);

fit::result<Errno, size_t> sys_pwritev(const CurrentTask& current_task, FdNumber fd,
                                       starnix_uapi::UserAddress iovec_addr,
                                       starnix_uapi::UserValue<uint32_t> iovec_count, off_t offset);

fit::result<Errno, size_t> sys_pwritev2(const CurrentTask& current_task, FdNumber fd,
                                        starnix_uapi::UserAddress iovec_addr,
                                        starnix_uapi::UserValue<uint32_t> iovec_count, off_t offset,
                                        uint64_t arg, uint32_t flags);

fit::result<Errno, FdNumber> sys_openat(const CurrentTask& current_task, FdNumber dir_fd,
                                        starnix_uapi::UserCString user_path, uint32_t flags,
                                        starnix_uapi::FileMode mode);

fit::result<Errno, FdNumber> sys_openat2(const CurrentTask& current_task, FdNumber dir_fd,
                                         starnix_uapi::UserCString, struct open_how, size_t size);

fit::result<Errno> sys_faccessat(const CurrentTask& current_task, FdNumber dir_fd,
                                 starnix_uapi::UserCString user_path, uint32_t mode);

fit::result<Errno> sys_faccessat2(const CurrentTask& current_task, FdNumber dir_fd,
                                  starnix_uapi::UserCString user_path, uint32_t mode);

fit::result<Errno, size_t> sys_getdents64(const CurrentTask& current_task, FdNumber fd,
                                          starnix_uapi::UserAddress user_buffer,
                                          size_t user_capacity);

fit::result<Errno> sys_chroot(const CurrentTask& current_task, starnix_uapi::UserCString user_path);

fit::result<Errno> sys_chdir(const CurrentTask& current_task, starnix_uapi::UserCString user_path);

fit::result<Errno> sys_fchdir(const CurrentTask& current_task, FdNumber fd);

fit::result<Errno> sys_fstat(const CurrentTask& current_task, FdNumber fd,
                             starnix_uapi::UserRef<struct ::stat> buffer);

fit::result<Errno> sys_newfstatat(const CurrentTask& current_task, FdNumber fd,
                                  starnix_uapi::UserCString user_path,
                                  starnix_uapi::UserRef<struct ::stat> buffer, uint32_t flags);

fit::result<Errno> sys_statx(const CurrentTask& current_task, FdNumber dir_fd,
                             starnix_uapi::UserCString user_path, uint32_t flags, uint32_t mask,
                             starnix_uapi::UserRef<struct ::statx> statxbuf);

fit::result<Errno, size_t> sys_readlinkat(const CurrentTask& current_task, FdNumber dir_fd,
                                          starnix_uapi::UserCString user_path,
                                          starnix_uapi::UserAddress buffer, size_t buffer_size);

fit::result<Errno> sys_mkdirat(const CurrentTask& current_task, FdNumber dir_fd,
                               starnix_uapi::UserCString user_path, starnix_uapi::FileMode mode);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYSCALLS_H_
