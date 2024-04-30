// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SYSCALLS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SYSCALLS_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix_uapi/errors.h>

namespace starnix {

fit::result<Errno, pid_t> sys_getpid(const CurrentTask& current_task);
fit::result<Errno, pid_t> sys_gettid(const CurrentTask& current_task);
fit::result<Errno, pid_t> sys_getppid(const CurrentTask& current_task);
fit::result<Errno, pid_t> sys_getpgid(const CurrentTask& current_task, pid_t pid);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SYSCALLS_H_
