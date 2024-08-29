// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SYSCALLS_MISC_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SYSCALLS_MISC_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>

namespace starnix {

fit::result<Errno, size_t> sys_getrandom(const CurrentTask& current_task,
                                         starnix_uapi::UserAddress buf_addr, size_t size,
                                         uint32_t flags);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SYSCALLS_MISC_H_
