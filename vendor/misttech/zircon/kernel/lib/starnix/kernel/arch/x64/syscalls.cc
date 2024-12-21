// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/arch/x64/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/syscalls.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/signals.h>

#include <asm/prctl.h>
#include <linux/errno.h>
#include <linux/sched.h>

namespace starnix {

fit::result<Errno> sys_arch_prctl(CurrentTask& current_task, uint32_t code,
                                  starnix_uapi::UserAddress addr) {
  switch (code) {
    case ARCH_SET_FS:
      current_task.thread_state().registers->fs_base = static_cast<uint64_t>(addr.ptr());
      return fit::ok();
    case ARCH_SET_GS:
      current_task.thread_state().registers->gs_base = static_cast<uint64_t>(addr.ptr());
      return fit::ok();
    default:
      return fit::error(errno(ENOSYS));
  }
}

fit::result<Errno> sys_chmod(const CurrentTask& current_task, starnix_uapi::UserCString user_path,
                             starnix_uapi::FileMode mode) {
  return sys_fchmodat(current_task, FdNumber::AT_FDCWD_, user_path, mode);
}

/// The parameter order for `clone` varies by architecture.
fit::result<Errno, pid_t> sys_clone(CurrentTask& current_task, uint64_t flags,
                                    starnix_uapi::UserAddress user_stack,
                                    starnix_uapi::UserRef<pid_t> user_parent_tid,
                                    starnix_uapi::UserRef<pid_t> user_child_tid,
                                    starnix_uapi::UserAddress user_tls) {
  // Our flags parameter uses the low 8 bits (CSIGNAL mask) of flags to indicate the exit
  // signal. The CloneArgs struct separates these as `flags` and `exit_signal`.
  return do_clone(current_task, {.flags = flags & ~static_cast<uint64_t>(CSIGNAL),
                                 .child_tid = user_child_tid.addr().ptr(),
                                 .parent_tid = user_parent_tid.addr().ptr(),
                                 .exit_signal = flags & static_cast<uint64_t>(CSIGNAL),
                                 .stack = user_stack.ptr(),
                                 .tls = user_tls.ptr()});
}

fit::result<Errno, pid_t> sys_fork(CurrentTask& current_task) {
  return do_clone(current_task, {
                                    .exit_signal = kSIGCHLD.number(),
                                });
}

// https://pubs.opengroup.org/onlinepubs/9699919799/functions/creat.html
fit::result<Errno, FdNumber> sys_creat(const CurrentTask& current_task,
                                       starnix_uapi::UserCString user_path,
                                       starnix_uapi::FileMode mode) {
  return sys_open(current_task, user_path,
                  (OpenFlags(OpenFlagsEnum::WRONLY) | OpenFlags(OpenFlagsEnum::CREAT) |
                   OpenFlags(OpenFlagsEnum::TRUNC))
                      .bits(),
                  mode);
}

fit::result<Errno, FdNumber> sys_dup2(const CurrentTask& current_task, FdNumber oldfd,
                                      FdNumber newfd) {
  if (oldfd == newfd) {
    // O_PATH allowed for:
    //
    //  Duplicating the file descriptor (dup(2), fcntl(2)
    //  F_DUPFD, etc.).
    //
    // See https://man7.org/linux/man-pages/man2/open.2.html
    _EP(current_task->files_.get_allowing_opath(oldfd));
    return fit::ok(newfd);
  }
  return sys_dup3(current_task, oldfd, newfd, 0);
}

fit::result<Errno, pid_t> sys_getpgrp(const CurrentTask& current_task) {
  return fit::ok(current_task->thread_group_->Read()->process_group_->leader_);
}

fit::result<Errno> sys_link(const CurrentTask& current_task,
                            starnix_uapi::UserCString old_user_path,
                            starnix_uapi::UserCString new_user_path) {
  return sys_linkat(current_task, FdNumber::AT_FDCWD_, old_user_path, FdNumber::AT_FDCWD_,
                    new_user_path, 0);
}

fit::result<Errno, FdNumber> sys_open(const CurrentTask& current_task,
                                      starnix_uapi::UserCString user_path, uint32_t flags,
                                      starnix_uapi::FileMode mode) {
  return sys_openat(current_task, FdNumber::AT_FDCWD_, user_path, flags, mode);
}

fit::result<Errno, size_t> sys_readlink(const CurrentTask& current_task,
                                        starnix_uapi::UserCString user_path,
                                        starnix_uapi::UserAddress buffer, size_t buffer_size) {
  return sys_readlinkat(current_task, FdNumber::AT_FDCWD_, user_path, buffer, buffer_size);
}

fit::result<Errno> sys_rmdir(const CurrentTask& current_task, starnix_uapi::UserCString user_path) {
  return sys_unlinkat(current_task, FdNumber::AT_FDCWD_, user_path, AT_REMOVEDIR);
}

fit::result<Errno> sys_rename(const CurrentTask& current_task,
                              starnix_uapi::UserCString old_user_path,
                              starnix_uapi::UserCString new_user_path) {
  return sys_renameat2(current_task, FdNumber::AT_FDCWD_, old_user_path, FdNumber::AT_FDCWD_,
                       new_user_path, 0);
}

fit::result<Errno> sys_renameat(const CurrentTask& current_task, FdNumber old_dir_fd,
                                starnix_uapi::UserCString old_user_path, FdNumber new_dir_fd,
                                starnix_uapi::UserCString new_user_path) {
  return sys_renameat2(current_task, old_dir_fd, old_user_path, new_dir_fd, new_user_path, 0);
}

fit::result<Errno> sys_stat(const CurrentTask& current_task, starnix_uapi::UserCString user_path,
                            starnix_uapi::UserRef<struct ::stat> buffer) {
  // TODO(https://fxbug.dev/42172993): Add the `AT_NO_AUTOMOUNT` flag once it is supported in
  // `sys_newfstatat`.
  return sys_newfstatat(current_task, FdNumber::AT_FDCWD_, user_path, buffer, 0);
}

fit::result<Errno> sys_symlink(const CurrentTask& current_task,
                               starnix_uapi::UserCString user_target,
                               starnix_uapi::UserCString user_path) {
  return sys_symlinkat(current_task, user_target, FdNumber::AT_FDCWD_, user_path);
}

fit::result<Errno, __kernel_time_t> sys_time(const CurrentTask& current_task,
                                             starnix_uapi::UserRef<__kernel_time_t> time_addr) {
  return fit::error(errno(ENOSYS));
}

fit::result<Errno> sys_unlink(const CurrentTask& current_task,
                              starnix_uapi::UserCString user_path) {
  return sys_unlinkat(current_task, FdNumber::AT_FDCWD_, user_path, 0);
}

fit::result<Errno, pid_t> sys_vfork(CurrentTask& current_task) {
  return do_clone(current_task, {
                                    .flags = (CLONE_VFORK | CLONE_VM),
                                    .exit_signal = kSIGCHLD.number(),
                                });
}

}  // namespace starnix
