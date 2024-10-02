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
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/error_propagation.h>
#include <trace.h>

#include <ktl/optional.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#include <linux/fcntl.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

namespace {

/// A convenient wrapper for Task::open_file_at.
///
/// Reads user_path from user memory and then calls through to Task::open_file_at.
fit::result<Errno, FileHandle> open_file_at(const CurrentTask& current_task, FdNumber dir_fd,
                                            UserCString user_path, uint32_t flags, FileMode mode,
                                            ResolveFlags resolve_flags) {
  auto path = current_task.read_c_string_to_vec(user_path, PATH_MAX) _EP(path);
  LTRACEF("dir_fd %d, path %s\n", dir_fd.raw(), path->c_str());
  return current_task.open_file_at(dir_fd, path.value(), OpenFlags::from_bits_truncate(flags), mode,
                                   resolve_flags);
}

FdFlags get_fd_flags(uint32_t flags) {
  if ((flags & O_CLOEXEC) != 0) {
    return FdFlags(FdFlagsEnum::CLOEXEC);
  }
  return FdFlags::empty();
}

fit::result<Errno, FdNumber> do_openat(const CurrentTask& current_task, FdNumber dir_fd,
                                       UserCString user_path, uint32_t flags, FileMode mode,
                                       ResolveFlags resolve_flags) {
  auto file = open_file_at(current_task, dir_fd, user_path, flags, mode, resolve_flags) _EP(file);
  auto fd_flags = get_fd_flags(flags);
  return current_task->add_file(file.value(), fd_flags);
}

fit::result<Errno, size_t> do_writev(const CurrentTask& current_task, FdNumber fd,
                                     starnix_uapi::UserAddress iovec_addr,
                                     starnix_uapi::UserValue<uint32_t> iovec_count,
                                     ktl::optional<off_t> offset, uint32_t flags) {
  if ((flags & ~RWF_SUPPORTED) != 0) {
    return fit::error(errno(EOPNOTSUPP));
  }

  if (flags != 0) {
    // track_stub !(TODO("https://fxbug.dev/322874523"), "pwritev2 flags", flags);
  }

  auto file = current_task->files.get(fd) _EP(file);
  auto iovec = current_task.read_iovec(iovec_addr, iovec_count) _EP(iovec);
  auto data = UserBuffersInputBuffer<TaskMemoryAccessor>::unified_new(
      current_task, ktl::move(iovec.value())) _EP(data);

  auto res = [&]() -> fit::result<Errno, size_t> {
    if (offset.has_value()) {
      return file->write_at(current_task, offset.value(), &data.value());
    }
    return file->write(current_task, &data.value());
  }();

  if (res.is_error()) {
    if (res.error_value().error_code() == EFAULT) {
      // track_stub!(TODO("https://fxbug.dev/297370529"), "allow partial writes");
    }
  }

  return res;
}

}  // namespace

fit::result<Errno, size_t> sys_read(const CurrentTask& current_task, FdNumber fd,
                                    starnix_uapi::UserAddress address, size_t length) {
  auto file = current_task->files.get(fd) _EP(file);
  auto buffer =
      UserBuffersOutputBuffer<TaskMemoryAccessor>::unified_new_at(current_task, address, length);
  return map_eintr(file->read(current_task, &*buffer), errno(ERESTARTSYS));
}

fit::result<Errno, size_t> sys_write(const CurrentTask& current_task, FdNumber fd,
                                     starnix_uapi::UserAddress address, size_t length) {
  auto file = current_task->files.get(fd) _EP(file);
  auto buffer = UserBuffersInputBuffer<TaskMemoryAccessor>::unified_new_at(current_task, address,
                                                                           length) _EP(buffer);
  return map_eintr(file->write(current_task, &*buffer), errno(ERESTARTSYS));
}

fit::result<Errno> sys_close(const CurrentTask& current_task, FdNumber fd) {
  auto result = current_task->files.close(fd) _EP(result);
  return fit::ok();
}

fit::result<Errno, size_t> sys_writev(const CurrentTask& current_task, FdNumber fd,
                                      starnix_uapi::UserAddress iovec_addr,
                                      starnix_uapi::UserValue<uint32_t> iovec_count) {
  return do_writev(current_task, fd, iovec_addr, iovec_count, ktl::nullopt, 0);
}

fit::result<Errno, FdNumber> sys_openat(const CurrentTask& current_task, FdNumber dir_fd,
                                        UserCString user_path, uint32_t flags, FileMode mode) {
  return do_openat(current_task, dir_fd, user_path, flags, mode, ResolveFlags::empty());
}

}  // namespace starnix
