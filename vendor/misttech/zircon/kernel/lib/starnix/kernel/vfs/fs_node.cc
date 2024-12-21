// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_node.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <trace.h>
#include <zircon/assert.h>

#include <optional>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

#include "../kernel_priv.h"

namespace ktl {

using std::addressof;
using std::destroy_at;

}  // namespace ktl

#include <ktl/enforce.h>

#include <linux/errno.h>
#include <linux/stat.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

using starnix_uapi::Access;
using starnix_uapi::AccessEnum;

FsNode* FsNode::new_root(FsNodeOps* ops) {
  return FsNode::new_root_with_properties(ops, [](FsNodeInfo& info) {});
}

FsNodeHandle FsNode::new_uncached(const CurrentTask& current_task, ktl::unique_ptr<FsNodeOps> ops,
                                  const FileSystemHandle& fs, ino_t node_id, FsNodeInfo info) {
  auto creds = current_task->creds();
  return FsNode::new_internal(ktl::move(ops), fs->kernel_, fs->weak_factory_.GetWeakPtr(), node_id,
                              info, creds)
      ->into_handle();
}

FsNodeHandle FsNode::new_uncached_with_creds(ktl::unique_ptr<FsNodeOps> ops,
                                             const FileSystemHandle& fs, ino_t node_id,
                                             FsNodeInfo info, const Credentials& credentials) {
  return FsNode::new_internal(ktl::move(ops), fs->kernel_, fs->weak_factory_.GetWeakPtr(), node_id,
                              info, credentials)
      ->into_handle();
}

FsNode* FsNode::new_internal(ktl::unique_ptr<FsNodeOps> ops, mtl::WeakPtr<Kernel> kernel,
                             mtl::WeakPtr<FileSystem> fs, ino_t node_id, FsNodeInfo info,
                             const Credentials& credentials) {
  // Allow the FsNodeOps to populate initial info.
  auto new_info = info;
  ops->initial_info(new_info);

  /*
  let fifo = if info.mode.is_fifo() {
      let mut default_pipe_capacity = (*PAGE_SIZE * 16) as usize;
      if !credentials.has_capability(CAP_SYS_RESOURCE) {
          let kernel = kernel.upgrade().expect("Invalid kernel when creating fs node");
          let max_size = kernel.system_limits.pipe_max_size.load(Ordering::Relaxed);
          default_pipe_capacity = std::cmp::min(default_pipe_capacity, max_size);
      }

      Some(Pipe::new(default_pipe_capacity))
  } else {
      None
  };
  */

  fbl::AllocChecker ac;
  auto fsnode = new (&ac) FsNode(ktl::move(WeakFsNodeHandle()), ktl::move(kernel), ktl::move(ops),
                                 ktl::move(fs), node_id, ktl::nullopt, new_info);
  ZX_ASSERT(ac.check());
  return fsnode;
}

FsNode::~FsNode() {
  LTRACE_ENTRY_OBJ;
  if (auto fs = fs_.Lock()) {
    fs->remove_node(*this);
  }

  // auto result = ops_->forget(*this, CurrentTask::Get());
  // if (result.is_error()) {
  //   log_error("Error on FsNodeOps::forget: %d", result.error_value());
  // }
  LTRACE_EXIT_OBJ;
}

FsNode::FsNode(WeakFsNodeHandle weak_handle, mtl::WeakPtr<Kernel> kernel,
               ktl::unique_ptr<FsNodeOps> ops, mtl::WeakPtr<FileSystem> fs, ino_t node_id,
               ktl::optional<PipeHandle> fifo, FsNodeInfo info)
    : weak_handle_(ktl::move(weak_handle)),
      ops_(ktl::move(ops)),
      kernel_(ktl::move(kernel)),
      fs_(ktl::move(fs)),
      node_id_(node_id),
      fifo_(ktl::move(fifo)),
      info_(ktl::move(info)),
      weak_factory_(this) {
  LTRACE_ENTRY_OBJ;
}

fit::result<Errno, ktl::unique_ptr<FileOps>> FsNode::create_file_ops(
    const CurrentTask& current_task, OpenFlags flags) const {
  return ops_->create_file_ops(*this, current_task, flags);
}

fit::result<Errno, ktl::unique_ptr<FileOps>> FsNode::open(const CurrentTask& current_task,
                                                          const MountInfo& mount, OpenFlags flags,
                                                          AccessCheck access_check) const {
  // If O_PATH is set, there is no need to create a real FileOps because
  // most file operations are disabled.
  if (flags.contains(OpenFlagsEnum::PATH)) {
    return fit::ok(ktl::move(ktl::unique_ptr<OPathOps>(OPathOps::New())));
  }

  auto access = access_check.resolve(flags);
  if (access.is_nontrivial()) {
    if (flags.contains(OpenFlagsEnum::NOATIME)) {
      // self.check_o_noatime_allowed(current_task) ? ;
    }

    _EP(check_access(current_task, mount, Access::from_open_flags(flags),
                     CheckAccessReason::InternalPermissionChecks));
  }

  auto [mode, rdev] = [&]() -> auto {
    // Don't hold the info lock while calling into open_device or self.ops().
    // TODO: The mode and rdev are immutable and shouldn't require a lock to read.
    auto info = this->info();
    return ktl::pair(info->mode_, info->rdev_);
  }();

  // `flags` doesn't contain any information about the EXEC permission. Instead the syscalls
  // used to execute a file (`sys_execve` and `sys_execveat`) call `open()` with the EXEC
  // permission request in `access`.
  // auto permission_flags = flags.contains(OpenFlagsEnum::APPEND) ?
  // PermissionFlags::APPEND :
  // PermissionFlags::empty();

  // security::fs_node_permission(current_task, self, permission_flags | access.into())?;

  auto fmt_mode = (mode & FileMode::IFMT);
  if (fmt_mode == FileMode::IFCHR || fmt_mode == FileMode::IFBLK || fmt_mode == FileMode::IFIFO) {
    return fit::error(errno(ENOTSUP));
  }
  if (fmt_mode == FileMode::IFSOCK) {
    // UNIX domain sockets can't be opened.
    return fit::error(errno(ENXIO));
  }
  return create_file_ops(current_task, flags);
}

fit::result<Errno, FsNodeHandle> FsNode::lookup(const CurrentTask& current_task,
                                                const MountInfo& mount, const FsStr& name) const {
  LTRACEF("name[%.*s]\n", static_cast<int>(name.size()), name.data());
  // self.check_access(current_task, mount, Access::EXEC)?;
  return ops_->lookup(*this, current_task, name);
}

fit::result<Errno, FsNodeHandle> FsNode::mknod(const CurrentTask& current_task,
                                               const MountInfo& mount, const FsStr& name,
                                               FileMode mode, DeviceType dev, FsCred owner) const {
  ASSERT_MSG((mode & FileMode::IFMT) != FileMode::EMPTY, "mknod called without node type.");
  _EP(check_access(current_task, mount, Access(AccessEnum::WRITE),
                   CheckAccessReason::InternalPermissionChecks));

  if (mode.is_reg()) {
    //_EP(security::check_fs_node_create_access(current_task, *this, mode));
  } else if (mode.is_dir()) {
    // Even though the man page for mknod(2) says that mknod "cannot be used to create
    // directories" in starnix the mkdir syscall (`sys_mkdirat`) ends up calling mknod.
    //_EP(security::check_fs_node_mkdir_access(current_task, *this, mode));
  } else if (!((mode.fmt() == FileMode::IFCHR) || (mode.fmt() == FileMode::IFBLK) ||
               (mode.fmt() == FileMode::IFIFO) || (mode.fmt() == FileMode::IFSOCK))) {
    //_EP(security::check_fs_node_mknod_access(current_task, *this, mode, dev));
  }

  update_metadata_for_child(current_task, mode, owner);

  FsNodeHandle new_node;
  if (mode.is_dir()) {
    auto result = ops_->mkdir(*this, current_task, name, mode, owner) _EP(result);
    new_node = result.value();
  } else {
    // https://man7.org/linux/man-pages/man2/mknod.2.html says on error EPERM:
    //
    //   mode requested creation of something other than a regular
    //   file, FIFO (named pipe), or UNIX domain socket, and the
    //   caller is not privileged (Linux: does not have the
    //   CAP_MKNOD capability); also returned if the filesystem
    //   containing pathname does not support the type of node
    //   requested.
    auto creds = current_task->creds();
    if (!creds.has_capability(kCapMknod)) {
      auto fmt = mode.fmt();
      if (fmt != FileMode::IFREG && fmt != FileMode::IFIFO && fmt != FileMode::IFSOCK) {
        return fit::error(errno(EPERM));
      }
    }
    auto result = ops_->mknod(*this, current_task, name, mode, dev, owner) _EP(result);
    new_node = result.value();
  }

  // self.init_new_node_security_on_create(locked, current_task, &new_node)?;

  return fit::ok(new_node);
}

fit::result<Errno, FsNodeHandle> FsNode::create_symlink(const CurrentTask& current_task,
                                                        const MountInfo& mount, const FsStr& name,
                                                        const FsStr& target, FsCred owner) const {
  _EP(check_access(current_task, mount, Access(AccessEnum::WRITE),
                   CheckAccessReason::InternalPermissionChecks));
  // security::check_fs_node_symlink_access(current_task, self, target)?;

  auto new_node = ops_->create_symlink(*this, current_task, name, target, owner) _EP(new_node);

  // self.init_new_node_security_on_create(&mut locked, current_task, &new_node)?;

  return fit::ok(new_node.value());
}

fit::result<Errno, FsNodeHandle> FsNode::create_tmpfile(const CurrentTask& current_task,
                                                        const MountInfo& mount, FileMode& mode,
                                                        FsCred& owner,
                                                        FsNodeLinkBehavior link_behavior) const {
  _EP(check_access(current_task, mount, Access(AccessEnum::WRITE),
                   CheckAccessReason::InternalPermissionChecks));
  update_metadata_for_child(current_task, mode, owner);

  auto node = ops_->create_tmpfile(*this, current_task, mode, owner) _EP(node);
  // security::fs_node_init_on_create(current_task, &node, self)?;
  (*node)->link_behavior_.set(link_behavior);
  return fit::ok(node.value());
}

fit::result<Errno, SymlinkTarget> FsNode::readlink(const CurrentTask& current_task) const {
  // TODO: 378864856 - Is there a permission check here other than security checks?
  // security::check_fs_node_read_link_access(current_task, self)?;
  return ops_->readlink(*this, current_task);
}

fit::result<Errno, FsNodeHandle> FsNode::link(const CurrentTask& current_task,
                                              const MountInfo& mount, const FsStr& name,
                                              const FsNodeHandle& child) const {
  _EP(check_access(current_task, mount, Access(AccessEnum::WRITE),
                   CheckAccessReason::InternalPermissionChecks));

  if (child->is_dir()) {
    return fit::error(errno(EPERM));
  }

  if (child->link_behavior_.get() == FsNodeLinkBehavior::kDisallowed) {
    return fit::error(errno(ENOENT));
  }

  // Check that current_task has permission to create the hard link.
  // See description of /proc/sys/fs/protected_hardlinks in
  // https://man7.org/linux/man-pages/man5/proc.5.html for details of security vulnerabilities.
  auto creds = current_task->creds();
  uid_t child_uid;
  FileMode mode;
  {
    auto info = child->info();
    child_uid = info->uid_;
    mode = info->mode_;
  }

  // Check that the filesystem UID of calling process matches the UID of existing file.
  // Check can be bypassed if calling process has CAP_FOWNER capability.
  if (!creds.has_capability(kCapFowner) && child_uid != creds.fsuid_) {
    // If current_task is not the user of existing file, needs read+write access
    auto access_result = child->check_access(
        current_task, mount, Access(Access(AccessEnum::READ) | Access(AccessEnum::WRITE)),
        CheckAccessReason::InternalPermissionChecks);
    if (access_result.is_error()) {
      // check_access returns EACCES when access rights don't match - change to EPERM
      if (access_result.error_value().error_code() == EACCES) {
        return fit::error(errno(EPERM));
      }
      return access_result.take_error();
    }

    // Security issues can arise when linking to setuid, setgid or special files
    if (mode.contains(FileMode::ISGID | FileMode::IXGRP)) {
      return fit::error(errno(EPERM));
    }
    if (mode.contains(FileMode::ISUID)) {
      return fit::error(errno(EPERM));
    }
    if (!mode.contains(FileMode::IFREG)) {
      return fit::error(errno(EPERM));
    }
  }

  // security::check_fs_node_link_access(current_task, self, child)?;

  _EP(ops_->link(*this, current_task, name, child));
  return fit::ok(child);
}

fit::result<Errno> FsNode::unlink(const CurrentTask& current_task, const MountInfo& mount,
                                  const FsStr& name, const FsNodeHandle& child) const {
  _EP(check_access(current_task, mount,
                   Access(Access(AccessEnum::WRITE) | Access(AccessEnum::EXEC)),
                   CheckAccessReason::InternalPermissionChecks));
  _EP(check_sticky_bit(current_task, child));

  if (child->is_dir()) {
    // security::check_fs_node_rmdir_access(current_task, self, child)?;
  } else {
    // security::check_fs_node_unlink_access(current_task, self, child)?;
  }

  _EP(ops_->unlink(*this, current_task, name, child));
  // update_ctime_mtime();
  return fit::ok();
}

fit::result<Errno> FsNode::default_check_access_impl(
    const CurrentTask& current_task, Access access,
    starnix_sync::RwLockGuard<FsNodeInfo, BrwLockPi::Reader> info) {
  auto [node_uid, node_gid, mode] = ktl::tuple(info->uid_, info->gid_, info->mode_);
  ktl::destroy_at(ktl::addressof(info));
  return current_task->creds().check_access(access, node_uid, node_gid, mode);
}

void FsNode::update_metadata_for_child(const CurrentTask& current_task, FileMode& mode,
                                       FsCred& owner) const {
  // The setgid bit on a directory causes the gid to be inherited by new children and the
  // setgid bit to be inherited by new child directories. See SetgidDirTest in gvisor.
  {
    auto self_info = info();
    if (self_info->mode_.contains(FileMode::ISGID)) {
      owner.gid_ = self_info->gid_;
      if (mode.is_dir()) {
        mode |= FileMode::ISGID;
      }
    }
  }

  if (!mode.is_dir()) {
    // https://man7.org/linux/man-pages/man7/inode.7.html says:
    //
    //   For an executable file, the set-group-ID bit causes the
    //   effective group ID of a process that executes the file to change
    //   as described in execve(2).
    //
    // We need to check whether the current task has permission to create such a file.
    // See a similar check in `FsNode::chmod`.
    auto creds = current_task->creds();
    if (!creds.has_capability(kCapFowner) && owner.gid_ != creds.fsgid_ &&
        !creds.is_in_group(owner.gid_)) {
      mode &= ~FileMode::ISGID;
    }
  }
}

fit::result<Errno> FsNode::check_access(const CurrentTask& current_task, const MountInfo& mount,
                                        Access access, CheckAccessReason reason) const {
  if (access.contains(AccessEnum::WRITE)) {
    _EP(mount.check_readonly_filesystem());
  }

  if (access.contains(AccessEnum::EXEC) && !is_dir()) {
    _EP(mount.check_noexec_filesystem());
  }

  return ops_->check_access(*this, current_task, access, info_, reason);
}

fit::result<Errno> FsNode::check_sticky_bit(const CurrentTask& current_task,
                                            const FsNodeHandle& child) const {
  auto creds = current_task->creds();
  if (!creds.has_capability(kCapFowner) && info()->mode_.contains(FileMode::ISVTX) &&
      child->info()->uid_ != creds.fsuid_) {
    return fit::error(errno(EPERM));
  }
  return fit::ok();
}

fit::result<Errno> FsNode::chmod(const CurrentTask& current_task, const MountInfo& mount,
                                 FileMode mode) const {
  _EP(mount.check_readonly_filesystem());

  auto update_attributes_fn = [&](FsNodeInfo& info) -> fit::result<Errno> {
    auto creds = current_task->creds();
    if (!creds.has_capability(kCapFowner)) {
      if (info.uid_ != creds.euid_) {
        return fit::error(errno(EPERM));
      }
      if (info.gid_ != creds.egid_ && !creds.is_in_group(info.gid_)) {
        mode &= ~FileMode::ISGID;
      }
    }
    info.chmod(mode);
    return fit::ok();
  };

  return update_attributes(current_task, update_attributes_fn);
}

fit::result<Errno, struct stat> FsNode::stat(const CurrentTask& current_task) const {
  auto result = fetch_and_refresh_info(current_task) _EP(result);
  auto info = ktl::move(result.value());

  /*
    let time_to_kernel_timespec_pair = |t| {
        let timespec { tv_sec, tv_nsec } = timespec_from_time(t);
        let time = tv_sec.try_into().map_err(|_| errno!(EINVAL))?;
        let time_nsec = tv_nsec.try_into().map_err(|_| errno!(EINVAL))?;
        Ok((time, time_nsec))
    };

    let (st_atime, st_atime_nsec) = time_to_kernel_timespec_pair(info.time_access)?;
    let (st_mtime, st_mtime_nsec) = time_to_kernel_timespec_pair(info.time_modify)?;
    let (st_ctime, st_ctime_nsec) = time_to_kernel_timespec_pair(info.time_status_change)?;
  */

  return fit::ok<struct stat>({
      //.st_dev =
      .st_ino = info->ino_,
      .st_nlink = static_cast<__kernel_ulong_t>(info->link_count_),
      .st_mode = info->mode_.bits(),
      .st_uid = info->uid_,
      .st_gid = info->gid_,
      .st_rdev = info->rdev_.bits(),
      .st_size = static_cast<__kernel_long_t>(info->size_),
      .st_blksize = static_cast<__kernel_long_t>(info->blksize_),
      .st_blocks = static_cast<__kernel_long_t>(info->blocks_),
  });
}

fit::result<Errno, struct ::statx> FsNode::statx(const CurrentTask& current_task, StatxFlags flags,
                                                 uint32_t mask) const {
  // Ignore mask for now and fill in all of the fields
  auto result = [&]() -> fit::result<Errno, starnix_sync::RwLock<FsNodeInfo>::RwLockReadGuard> {
    if (flags.contains(StatxFlags(StatxFlagsEnum::_AT_STATX_DONT_SYNC))) {
      return fit::ok(ktl::move(info()));
    }
    auto r = fetch_and_refresh_info(current_task) _EP(r);
    return fit::ok(ktl::move(r.value()));
  }() _EP(result);

  starnix_sync::RwLock<FsNodeInfo>::RwLockReadGuard info = ktl::move(result.value());

  if ((mask & STATX__RESERVED) == STATX__RESERVED) {
    return fit::error(errno(EINVAL));
  }

  // TODO(https://fxbug.dev/302594110): statx attributes
  uint32_t stx_mnt_id = 0;
  uint64_t stx_attributes = 0;
  uint64_t stx_attributes_mask = STATX_ATTR_VERITY;

  // TODO: Port fsverity check once implemented
  /*if (matches!(*self.fsverity.lock(), FsVerityState::FsVerity)) {
    stx_attributes |= STATX_ATTR_VERITY;
  }*/

  return fit::ok<struct ::statx>(
      {.stx_mask = STATX_NLINK | STATX_UID |
                   STATX_GID /*| STATX_ATIME | STATX_MTIME | STATX_CTIME*/ | STATX_INO |
                   STATX_SIZE | STATX_BLOCKS | STATX_BASIC_STATS,
       .stx_blksize = static_cast<__u32>(info->blksize_),
       .stx_attributes = stx_attributes,
       .stx_nlink = static_cast<__u32>(info->link_count_),
       .stx_uid = info->uid_,
       .stx_gid = info->gid_,
       .stx_mode = static_cast<__u16>(info->mode_.bits()),
       .stx_ino = info->ino_,
       .stx_size = static_cast<__u64>(info->size_),
       .stx_blocks = static_cast<__u64>(info->blocks_),
       .stx_attributes_mask = stx_attributes_mask,
       // TODO: Port timestamp conversion
       //.stx_atime = statx_timestamp_from_time(info.time_access_),
       //.stx_mtime = statx_timestamp_from_time(info.time_modify_),
       //.stx_ctime = statx_timestamp_from_time(info.time_status_change_),

       .stx_rdev_major = info->rdev_.major(),
       .stx_rdev_minor = info->rdev_.minor(),

       //.stx_dev_major = fs_->dev_id().major(),
       //.stx_dev_minor = fs_->dev_id().minor(),
       .stx_mnt_id = stx_mnt_id});
}

fit::result<Errno, starnix_sync::RwLock<FsNodeInfo>::RwLockReadGuard>
FsNode::fetch_and_refresh_info(const CurrentTask& current_task) const {
  return ops().fetch_and_refresh_info(*this, current_task, info_);
}

}  // namespace starnix
