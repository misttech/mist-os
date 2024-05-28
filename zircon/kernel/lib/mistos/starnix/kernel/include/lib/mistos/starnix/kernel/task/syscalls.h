// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SYSCALLS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SYSCALLS_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/errors.h>

#include <linux/sched.h>

namespace starnix {

fit::result<Errno, pid_t> do_clone(const CurrentTask& current_task, struct clone_args);

fit::result<Errno, pid_t> sys_getpid(const CurrentTask& current_task);
fit::result<Errno, pid_t> sys_gettid(const CurrentTask& current_task);
fit::result<Errno, pid_t> sys_getppid(const CurrentTask& current_task);
fit::result<Errno, pid_t> sys_getsid(const CurrentTask& current_task, pid_t pid);
fit::result<Errno, pid_t> sys_getpgid(const CurrentTask& current_task, pid_t pid);

fit::result<Errno, uid_t> sys_getuid(const CurrentTask& current_task);
fit::result<Errno, uid_t> sys_getgid(const CurrentTask& current_task);
fit::result<Errno, uid_t> sys_geteuid(const CurrentTask& current_task);
fit::result<Errno, uid_t> sys_getegid(const CurrentTask& current_task);

fit::result<Errno, starnix_syscalls::SyscallResult> sys_prctl(const CurrentTask& current_task,
                                                              int option, uint64_t arg2,
                                                              uint64_t arg3, uint64_t arg4,
                                                              uint64_t arg5);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SYSCALLS_H_
