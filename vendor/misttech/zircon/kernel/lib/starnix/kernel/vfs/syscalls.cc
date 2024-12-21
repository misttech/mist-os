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
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/error_propagation.h>
#include <lib/mistos/util/try_from.h>
#include <trace.h>

#include <ktl/optional.h>
#include <ktl/span.h>
#include <ktl/variant.h>

#include "../kernel_priv.h"

namespace ktl {
using std::is_invocable_r_v;
}  // namespace ktl

#include <ktl/enforce.h>

#include <asm/ioctls.h>
#include <asm/stat.h>
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
                                   resolve_flags, AccessCheck());
}

template <typename T, typename CallbackFn>
fit::result<Errno, T> lookup_parent_at(const CurrentTask& current_task, FdNumber dir_fd,
                                       UserCString user_path, CallbackFn&& callback) {
  auto path = current_task.read_c_string_to_vec(user_path, PATH_MAX) _EP(path);
  LTRACEF("dir_fd %d, path %s\n", dir_fd.raw(), path->c_str());
  if (path->empty()) {
    return fit::error(errno(ENOENT));
  }
  auto context = LookupContext::Default();
  auto result = current_task.lookup_parent_at(context, dir_fd, path.value()) _EP(result);
  return callback(context, result->first, result->second);
}

template <typename CallbackFn>
fit::result<Errno> lookup_parent_at(const CurrentTask& current_task, FdNumber dir_fd,
                                    UserCString user_path, CallbackFn&& callback) {
  auto path = current_task.read_c_string_to_vec(user_path, PATH_MAX) _EP(path);
  LTRACEF("dir_fd %d, path %s\n", dir_fd.raw(), path->c_str());
  if (path->empty()) {
    return fit::error(errno(ENOENT));
  }
  auto context = LookupContext::Default();
  auto result = current_task.lookup_parent_at(context, dir_fd, path.value()) _EP(result);
  return callback(context, result->first, result->second);
}

struct LookupFlags {
  /// Whether AT_EMPTY_PATH was supplied.
  bool allow_empty_path = false;

  /// Used to implement AT_SYMLINK_NOFOLLOW.
  SymlinkMode symlink_mode = SymlinkMode::Follow;

  /// Automount directories on the path.
  // TODO(https://fxbug.dev/297370602): Support the `AT_NO_AUTOMOUNT` flag.
  bool automount = false;

  // impl LookupFlags
  static LookupFlags no_follow() { return LookupFlags{.symlink_mode = SymlinkMode::NoFollow}; }

  static fit::result<Errno, LookupFlags> from_bits(uint32_t flags, uint32_t allowed_flags) {
    if ((flags & ~allowed_flags) != 0) {
      return fit::error(errno(EINVAL));
    }
    auto follow_symlinks = [&]() {
      if ((allowed_flags & AT_SYMLINK_FOLLOW) != 0) {
        return (flags & AT_SYMLINK_FOLLOW) != 0;
      }
      return (flags & AT_SYMLINK_NOFOLLOW) == 0;
    }();

    auto automount = [&]() {
      if ((allowed_flags & AT_NO_AUTOMOUNT) != 0) {
        return (flags & AT_NO_AUTOMOUNT) != 0;
      }
      return false;
    }();

    if (automount) {
      // track_stub!(TODO("https://fxbug.dev/297370602"), "LookupFlags::automount");
    }

    return fit::ok(
        LookupFlags{.allow_empty_path = ((flags & AT_EMPTY_PATH) != 0) ||
                                        ((flags & O_PATH) != 0 && (flags & O_NOFOLLOW) != 0),
                    .symlink_mode = follow_symlinks ? SymlinkMode::Follow : SymlinkMode::NoFollow,
                    .automount = automount});
  }

  /// impl From<StatxFlags> for LookupFlags
  static LookupFlags from(StatxFlags flags) {
    auto lookup_flags = StatxFlags(StatxFlagsEnum::_AT_SYMLINK_NOFOLLOW) |
                        StatxFlags(StatxFlagsEnum::_AT_EMPTY_PATH) |
                        StatxFlags(StatxFlagsEnum::_AT_NO_AUTOMOUNT);
    return from_bits(flags.bits() & lookup_flags.bits(), lookup_flags.bits()).value();
  }
};

fit::result<Errno, NamespaceNode> lookup_at(const CurrentTask& current_task, FdNumber dir_fd,
                                            UserCString user_path, LookupFlags options) {
  auto path = current_task.read_c_string_to_vec(user_path, PATH_MAX) _EP(path);
  LTRACEF("dir_fd %d, path %s\n", dir_fd.raw(), path->c_str());
  if (path->empty()) {
    if (options.allow_empty_path) {
      auto pair =
          current_task.resolve_dir_fd(dir_fd, path.value(), ResolveFlags::empty()) _EP(pair);
      return fit::ok(pair->first);
    }
    return fit::error(errno(ENOENT));
  }

  auto parent_context = LookupContext::Default();
  auto result = current_task.lookup_parent_at(parent_context, dir_fd, path.value()) _EP(result);
  auto [parent, basename] = *result;

  auto child_context = [&]() {
    if (parent_context.must_be_directory) {
      // The child must resolve to a directory. This is because a trailing slash
      // was found in the path. If the child is a symlink, we should follow it.
      // See
      // https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
      return parent_context.with(SymlinkMode::Follow);
    }
    return parent_context.with(options.symlink_mode);
  }();

  return parent.lookup_child(current_task, child_context, basename);
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

  auto file = current_task->files_.get(fd) _EP(file);
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
  auto file = current_task->files_.get(fd) _EP(file);
  auto buffer =
      UserBuffersOutputBuffer<TaskMemoryAccessor>::unified_new_at(current_task, address, length);
  return map_eintr(file->read(current_task, &*buffer),
                   starnix_uapi::Errno::New(starnix_uapi::ERESTARTSYS));
}

fit::result<Errno, size_t> sys_write(const CurrentTask& current_task, FdNumber fd,
                                     starnix_uapi::UserAddress address, size_t length) {
  auto file = current_task->files_.get(fd) _EP(file);
  auto buffer = UserBuffersInputBuffer<TaskMemoryAccessor>::unified_new_at(current_task, address,
                                                                           length) _EP(buffer);
  return map_eintr(file->write(current_task, &*buffer),
                   starnix_uapi::Errno::New(starnix_uapi::ERESTARTSYS));
}

fit::result<Errno> sys_close(const CurrentTask& current_task, FdNumber fd) {
  auto result = current_task->files_.close(fd) _EP(result);
  return fit::ok();
}

fit::result<Errno, off_t> sys_lseek(const CurrentTask& current_task, FdNumber fd, off_t offset,
                                    uint32_t whence) {
  auto file = current_task->files_.get(fd) _EP(file);
  auto target = SeekTarget::from_raw(whence, offset) _EP(target);
  return file->seek(current_task, target.value());
}

fit::result<Errno, size_t> sys_pread64(const CurrentTask& current_task, FdNumber fd,
                                       starnix_uapi::UserAddress address, size_t length,
                                       off_t offset) {
  auto file = current_task->files_.get(fd) _EP(file);
  auto unsiged_offset = mtl::TryFrom<off_t, size_t>(offset);
  if (!unsiged_offset.has_value()) {
    return fit::error(errno(EINVAL));
  }
  auto buffer = UserBuffersOutputBuffer<TaskMemoryAccessor>::unified_new_at(current_task, address,
                                                                            length) _EP(buffer);
  return file->read_at(current_task, *unsiged_offset, &*buffer);
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

fit::result<Errno> sys_chroot(const CurrentTask& current_task,
                              starnix_uapi::UserCString user_path) {
  auto name = lookup_at(current_task, FdNumber::AT_FDCWD_, user_path, LookupFlags()) _EP(name);
  if (!name->entry_->node_->is_dir()) {
    return fit::error(errno(ENOTDIR));
  }

  auto result = current_task->fs()->chroot(current_task, name.value()) _EP(result);
  return fit::ok();
}

fit::result<Errno> sys_chdir(const CurrentTask& current_task, starnix_uapi::UserCString user_path) {
  auto name = lookup_at(current_task, FdNumber::AT_FDCWD_, user_path, LookupFlags()) _EP(name);
  if (!name->entry_->node_->is_dir()) {
    return fit::error(errno(ENOTDIR));
  }
  return current_task->fs()->chdir(current_task, name.value());
}

fit::result<Errno> sys_fstat(const CurrentTask& current_task, FdNumber fd,
                             starnix_uapi::UserRef<struct ::stat> buffer) {
  // O_PATH allowed for:
  //
  //   fstat(2) (since Linux 3.6).
  //
  // See https://man7.org/linux/man-pages/man2/open.2.html
  auto file = current_task->files_.get_allowing_opath(fd) _EP(file);
  auto result = file->node()->stat(current_task) _EP(result);
  auto write_result = current_task.write_object(buffer, *result) _EP(write_result);
  return fit::ok();
}

fit::result<Errno> sys_newfstatat(const CurrentTask& current_task, FdNumber dir_fd,
                                  starnix_uapi::UserCString user_path,
                                  starnix_uapi::UserRef<struct ::stat> buffer, uint32_t flags) {
  auto lflags = LookupFlags::from_bits(flags, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT)
      _EP(lflags);
  auto name = lookup_at(current_task, dir_fd, user_path, lflags.value()) _EP(name);
  auto result = name->entry_->node_->stat(current_task) _EP(result);
  return fit::ok();
}

fit::result<Errno> sys_statx(const CurrentTask& current_task, FdNumber dir_fd,
                             starnix_uapi::UserCString user_path, uint32_t flags, uint32_t mask,
                             starnix_uapi::UserRef<struct ::statx> statxbuf) {
  auto statx_flags = StatxFlags::from_bits(flags);
  if (!statx_flags) {
    return fit::error(errno(EINVAL));
  }

  if ((statx_flags.value() & (StatxFlags(StatxFlagsEnum::_AT_STATX_FORCE_SYNC) |
                              StatxFlags(StatxFlagsEnum::_AT_STATX_DONT_SYNC))) ==
      (StatxFlags(StatxFlagsEnum::_AT_STATX_FORCE_SYNC) |
       StatxFlags(StatxFlagsEnum::_AT_STATX_DONT_SYNC))) {
    return fit::error(errno(EINVAL));
  }

  auto name =
      lookup_at(current_task, dir_fd, user_path, LookupFlags::from(statx_flags.value())) _EP(name);
  auto result = name->entry_->node_->statx(current_task, statx_flags.value(), mask) _EP(result);
  _EP(current_task.write_object(statxbuf, *result));
  return fit::ok();
}

fit::result<Errno, size_t> sys_readlinkat(const CurrentTask& current_task, FdNumber dir_fd,
                                          starnix_uapi::UserCString user_path,
                                          starnix_uapi::UserAddress buffer, size_t buffer_size) {
  auto path = current_task.read_c_string_to_vec(user_path, static_cast<size_t>(PATH_MAX)) _EP(path);
  auto lookup_flags = [&]() -> fit::result<Errno, LookupFlags> {
    if (path->empty()) {
      if (dir_fd == FdNumber::AT_FDCWD_) {
        return fit::error(errno(ENOENT));
      }
      return fit::ok(LookupFlags{.allow_empty_path = true, .symlink_mode = SymlinkMode::NoFollow});
    }
    return fit::ok(LookupFlags::no_follow());
  }() _EP(lookup_flags);

  auto name = lookup_at(current_task, dir_fd, user_path, lookup_flags.value()) _EP(name);

  auto result = name->readlink(current_task) _EP(result);
  auto target = [&]() -> FsString {
    return ktl::visit(SymlinkTarget::overloaded{
                          [](FsString path) { return path; },
                          [&current_task](const NamespaceNode& node) {
                            return node.path(*current_task.operator->());
                          },
                      },
                      result->variant_);
  }();

  if (buffer_size == 0) {
    return fit::error(errno(EINVAL));
  }

  // Cap the returned length at buffer_size.
  auto length = ktl::min(buffer_size, target.size());
  _EP(current_task->write_memory(
      buffer, ktl::span<const uint8_t>(reinterpret_cast<const uint8_t*>(target.data()), length)));
  return fit::ok(length);
}

fit::result<Errno> sys_mkdirat(const CurrentTask& current_task, FdNumber dir_fd,
                               starnix_uapi::UserCString user_path, starnix_uapi::FileMode mode) {
  auto path = current_task.read_c_string_to_vec(user_path, static_cast<size_t>(PATH_MAX)) _EP(path);

  if (path->empty()) {
    return fit::error(errno(ENOENT));
  }
  auto lc = LookupContext::Default();
  auto result = current_task.lookup_parent_at(lc, dir_fd, path.value()) _EP(result);
  auto [parent, basename] = result.value();
  _EP(parent.create_node(current_task, basename, mode.with_type(FileMode::IFDIR),
                         DeviceType::NONE));
  return fit::ok();
}

fit::result<Errno> sys_linkat(const CurrentTask& current_task, FdNumber old_dir_fd,
                              starnix_uapi::UserCString old_user_path, FdNumber new_dir_fd,
                              starnix_uapi::UserCString new_user_path, uint32_t flags) {
  if ((flags & ~(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH)) != 0) {
    // TODO(https://fxbug.dev/322875706): Handle unknown linkat flags
    return fit::error(errno(EINVAL));
  }

  if ((flags & AT_EMPTY_PATH) && !current_task->creds().has_capability(kCapDacReadSearch)) {
    return fit::error(errno(ENOENT));
  }

  auto lookup_flags =
      LookupFlags::from_bits(flags, AT_EMPTY_PATH | AT_SYMLINK_FOLLOW) _EP(lookup_flags);

  auto target =
      lookup_at(current_task, old_dir_fd, old_user_path, lookup_flags.value()) _EP(target);

  auto result = lookup_parent_at<NamespaceNode>(
      current_task, new_dir_fd, new_user_path,
      [&](LookupContext context, NamespaceNode parent,
          const FsStr& basename) -> fit::result<Errno, NamespaceNode> {
        // The path to a new symlink cannot end in `/`. That would imply
        // that we are dereferencing the symlink to a directory.
        //
        // See
        // https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
        if (context.must_be_directory) {
          return fit::error(errno(ENOENT));
        }
        return parent.link(current_task, basename, target->entry_->node_);
      }) _EP(result);

  return fit::ok();
}

fit::result<Errno> sys_unlinkat(const CurrentTask& current_task, FdNumber dir_fd,
                                starnix_uapi::UserCString user_path, uint32_t flags) {
  if ((flags & ~AT_REMOVEDIR) != 0) {
    return fit::error(errno(EINVAL));
  }
  auto kind = (flags & AT_REMOVEDIR) ? UnlinkKind::Directory : UnlinkKind::NonDirectory;
  auto result = lookup_parent_at(current_task, dir_fd, user_path,
                                 [&](LookupContext context, NamespaceNode parent,
                                     const FsStr& basename) -> fit::result<Errno> {
                                   return parent.unlink(current_task, basename, kind,
                                                        context.must_be_directory);
                                 }) _EP(result);
  return fit::ok();
}

fit::result<Errno> sys_fchmod(const CurrentTask& current_task, FdNumber fd,
                              starnix_uapi::FileMode mode) {
  // Remove the filetype from the mode
  auto permissions_mode = mode & FileMode::PERMISSIONS;
  auto file = current_task->files_.get(fd) _EP(file);
  _EP(file->name_->entry_->node_->chmod(current_task, file->name_->mount_, permissions_mode));
  // file->name_->entry_.notify_ignoring_excl_unlink(InotifyMask::ATTRIB);
  return fit::ok();
}

fit::result<Errno> sys_fchmodat(const CurrentTask& current_task, FdNumber dir_fd,
                                starnix_uapi::UserCString user_path, starnix_uapi::FileMode mode) {
  auto permissions_mode = mode & FileMode::PERMISSIONS;
  auto name = lookup_at(current_task, dir_fd, user_path, LookupFlags()) _EP(name);
  _EP(name->entry_->node_->chmod(current_task, name->mount_, permissions_mode));
  // name->entry_.notify_ignoring_excl_unlink(InotifyMask::ATTRIB);
  return fit::ok();
}

fit::result<Errno, size_t> sys_getcwd(const CurrentTask& current_task,
                                      starnix_uapi::UserAddress buf, size_t size) {
  auto root = current_task->fs()->root();
  auto cwd = current_task->fs()->cwd();

  auto user_cwd = [&]() -> BString {
    auto path_result = cwd.path_from_root(root);
    return ktl::visit(PathWithReachability::overloaded{
                          [](const Reachable& r) { return r.path; },
                          [](const Unreachable& u) {
                            FsString combined;
                            // combined.append("(unreachable)", 12);
                            // combined.append(u.path);
                            return combined;
                          },
                      },
                      path_result.variant_);
  }();

  fbl::AllocChecker ac;
  user_cwd.push('\0', &ac);
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }

  if (user_cwd.size() > size) {
    return fit::error(errno(ERANGE));
  }

  _EP(current_task->write_memory(
      buf, ktl::span<const uint8_t>(reinterpret_cast<const uint8_t*>(user_cwd.data()),
                                    user_cwd.size())));

  return fit::ok(user_cwd.size());
}

fit::result<Errno, starnix_syscalls::SyscallResult> sys_ioctl(const CurrentTask& current_task,
                                                              FdNumber fd, uint32_t request,
                                                              starnix_syscalls::SyscallArg arg) {
  // TODO: https://fxbug.dev/364569179 - Figure out what to do about the security
  // check for FIONREAD, FIBMAP, FIGETBSZ, FIONBIO, and FIOASYNC. These ioctls check
  // a different set of permissions than other arbitrary ioctls.
  switch (request) {
    case FIOCLEX:
      _EP(current_task->files_.set_fd_flags(fd, FdFlags(FdFlagsEnum::CLOEXEC)));
      return fit::ok(starnix_syscalls::SUCCESS);
    case FIONCLEX:
      _EP(current_task->files_.set_fd_flags(fd, FdFlags::empty()));
      return fit::ok(starnix_syscalls::SUCCESS);
    default: {
      auto file = current_task->files_.get(fd) _EP(file);
      //_EP(security::check_file_ioctl_access(current_task, file));
      return file->ioctl(current_task, request, arg);
    }
  }
}

fit::result<Errno> sys_symlinkat(const CurrentTask& current_task,
                                 starnix_uapi::UserCString user_target, FdNumber new_dir_fd,
                                 starnix_uapi::UserCString user_path) {
  auto target = current_task->read_c_string_to_vec(user_target, PATH_MAX) _EP(target);
  if (target->empty()) {
    return fit::error(errno(ENOENT));
  }

  auto path = current_task->read_c_string_to_vec(user_path, PATH_MAX) _EP(path);
  if (path->empty()) {
    return fit::error(errno(ENOENT));
  }

  auto result = lookup_parent_at<NamespaceNode>(
      current_task, new_dir_fd, user_path,
      [&](LookupContext context, NamespaceNode parent,
          const FsStr& basename) -> fit::result<Errno, NamespaceNode> {
        // The path to a new symlink cannot end in `/`. That would imply
        // that we are dereferencing the symlink to a directory.
        //
        // See
        // https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
        if (context.must_be_directory) {
          return fit::error(errno(ENOENT));
        }
        return parent.create_symlink(current_task, basename, target.value());
      }) _EP(result);

  return fit::ok();
}

fit::result<Errno, FdNumber> sys_dup(const CurrentTask& current_task, FdNumber oldfd) {
  return current_task->files_.duplicate(*current_task, oldfd, TargetFdNumber::Default(),
                                        FdFlags::empty());
}

fit::result<Errno, FdNumber> sys_dup3(const CurrentTask& current_task, FdNumber oldfd,
                                      FdNumber newfd, uint32_t flags) {
  if (oldfd == newfd) {
    return fit::error(errno(EINVAL));
  }
  if ((flags & ~O_CLOEXEC) != 0) {
    return fit::error(errno(EINVAL));
  }
  auto fd_flags = get_fd_flags(flags);
  _EP(current_task->files_.duplicate(*current_task, oldfd, TargetFdNumber::Specific(newfd),
                                     fd_flags));
  return fit::ok(newfd);
}

}  // namespace starnix
