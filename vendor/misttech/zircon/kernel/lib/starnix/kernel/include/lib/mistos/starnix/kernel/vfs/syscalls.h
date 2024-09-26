// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYSCALLS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYSCALLS_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/user_address.h>

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

fit::result<Errno, FdNumber> sys_openat(const CurrentTask& current_task, FdNumber fir_fd,
                                        starnix_uapi::UserCString, uint32_t flags,
                                        starnix_uapi::FileMode mode);

fit::result<Errno, FdNumber> sys_openat2(const CurrentTask& current_task, FdNumber fir_fd,
                                         starnix_uapi::UserCString, struct open_how, size_t size);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYSCALLS_H_
