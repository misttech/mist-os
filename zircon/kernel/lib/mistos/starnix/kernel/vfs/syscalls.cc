// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix/kernel/vfs/syscalls.h>
#include <lib/mistos/starnix_uapi/open_flags.h>

#include <fbl/string.h>

#include <linux/fcntl.h>

using namespace starnix_uapi;

namespace starnix {

namespace {

/// A convenient wrapper for Task::open_file_at.
///
/// Reads user_path from user memory and then calls through to Task::open_file_at.
fit::result<Errno, FileHandle> open_file_at(const CurrentTask& current_task, FdNumber dir_fd,
                                            UserCString user_path, uint32_t flags, FileMode mode,
                                            ResolveFlags resolve_flags) {
  auto path_or_error = current_task.read_c_string_to_vec(user_path, PATH_MAX);
  if (path_or_error.is_error())
    return path_or_error.take_error();
  return current_task.open_file_at(dir_fd, path_or_error.value(),
                                   OpenFlags::from_bits_truncate(flags), mode, resolve_flags);
}

FdFlags get_fd_flags(uint32_t flags) {
  if ((flags & O_CLOEXEC) != 0) {
    return FdFlags(FdFlagsEnum::CLOEXEC);
  } else {
    return FdFlags::empty();
  }
}

fit::result<Errno, FdNumber> do_openat(const CurrentTask& current_task, FdNumber dir_fd,
                                       UserCString user_path, uint32_t flags, FileMode mode,
                                       ResolveFlags resolve_flags) {
  auto file_or_error = open_file_at(current_task, dir_fd, user_path, flags, mode, resolve_flags);
  if (file_or_error.is_error()) {
    return file_or_error.take_error();
  }
  auto fd_flags = get_fd_flags(flags);
  return current_task->add_file(file_or_error.value(), fd_flags);
}

}  // namespace

fit::result<Errno, size_t> sys_read(const CurrentTask& current_task, FdNumber fd,
                                    starnix_uapi::UserAddress address, size_t length) {
  auto file_or_error = current_task->files.get(fd);
  if (file_or_error.is_error())
    return file_or_error.take_error();
  // file_or_error->read(current_task, );
  return fit::error(errno(ENOSYS));
}

fit::result<Errno, size_t> sys_write(const CurrentTask& current_task, FdNumber fd,
                                     starnix_uapi::UserAddress address, size_t length) {
  auto file_or_error = current_task->files.get(fd);
  if (file_or_error.is_error())
    return file_or_error.take_error();
  // file_or_error->write(current_task, );
  return fit::error(errno(ENOSYS));
}

fit::result<Errno> sys_close(const CurrentTask& current_task, FdNumber fd) {
  return current_task->files.close(fd);
}

fit::result<Errno, FdNumber> sys_openat(const CurrentTask& current_task, FdNumber dir_fd,
                                        UserCString user_path, uint32_t flags, FileMode mode) {
  return do_openat(current_task, dir_fd, user_path, flags, mode, ResolveFlags::empty());
}

}  // namespace starnix
