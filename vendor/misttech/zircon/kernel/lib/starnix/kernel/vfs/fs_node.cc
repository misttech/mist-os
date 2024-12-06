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

#include <ktl/enforce.h>

#include <linux/errno.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

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
                                                          bool check_access) const {
  // If O_PATH is set, there is no need to create a real FileOps because
  // most file operations are disabled.
  if (flags.contains(OpenFlagsEnum::PATH)) {
    return fit::ok(ktl::move(ktl::unique_ptr<OPathOps>(OPathOps::New())));
  }

  if (check_access) {
    if (flags.contains(OpenFlagsEnum::NOATIME)) {
      // self.check_o_noatime_allowed(current_task) ? ;
    }
    // self.check_access(current_task, mount, Access::from_open_flags(flags)) ? ;
  }

  auto [mode, rdev] = [&]() -> auto {
    auto info = this->info();
    return ktl::pair(info->mode, info->rdev);
  }();

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
  /*
    self.check_access(current_task, mount, Access::WRITE)?;
    self.update_metadata_for_child(current_task, &mut mode, &mut owner);
  */

  if (mode.is_dir()) {
    return ops_->mkdir(*this, current_task, name, mode, owner);
  }
  // https://man7.org/linux/man-pages/man2/mknod.2.html says:
  //
  //   mode requested creation of something other than a regular
  //   file, FIFO (named pipe), or UNIX domain socket, and the
  //   caller is not privileged (Linux: does not have the
  //   CAP_MKNOD capability); also returned if the filesystem
  //   containing pathname does not support the type of node
  //   requested.

  /*
    let creds = current_task.creds();
    if !creds.has_capability(CAP_MKNOD) {
        if !matches!(mode.fmt(), FileMode::IFREG | FileMode::IFIFO | FileMode::IFSOCK) {
            return error!(EPERM);
        }
    }
    let mut locked = locked.cast_locked::<FileOpsCore>();
  */
  return ops_->mknod(*this, current_task, name, mode, dev, owner);
}

fit::result<Errno, SymlinkTarget> FsNode::readlink(const CurrentTask& current_task) const {
  // TODO(qsr): Is there a permission check here?
  return ops_->readlink(*this, current_task);
}

fit::result<Errno, struct stat> FsNode::stat(const CurrentTask& current_task) const {
  auto result = fetch_and_refresh_info(current_task) _EP(result);
  auto info = result.value();

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
      .st_ino = info.ino,
      .st_nlink = static_cast<__kernel_ulong_t>(info.link_count),
      .st_mode = info.mode.bits(),
      .st_uid = info.uid,
      .st_gid = info.gid,
      .st_rdev = info.rdev.bits(),
      .st_size = static_cast<__kernel_long_t>(info.size),
      .st_blksize = static_cast<__kernel_long_t>(info.blksize),
      .st_blocks = static_cast<__kernel_long_t>(info.blocks),
  });
}

fit::result<Errno, FsNodeInfo> FsNode::fetch_and_refresh_info(
    const CurrentTask& current_task) const {
  return ops().fetch_and_refresh_info(*this, current_task, info_);
}

}  // namespace starnix
