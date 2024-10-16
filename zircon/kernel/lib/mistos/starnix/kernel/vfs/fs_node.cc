// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_node.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <zircon/assert.h>

#include <optional>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

#include <linux/errno.h>

namespace starnix {

FsNode* FsNode::new_internal(ktl::unique_ptr<FsNodeOps> ops, util::WeakPtr<Kernel> kernel,
                             util::WeakPtr<FileSystem> fs, ino_t node_id, FsNodeInfo info,
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
                                 ktl::move(fs), node_id, std::nullopt, new_info);
  ZX_ASSERT(ac.check());
  return fsnode;
}

FsNode::FsNode(WeakFsNodeHandle _weak_handle, util::WeakPtr<Kernel> kernel,
               ktl::unique_ptr<FsNodeOps> ops, util::WeakPtr<FileSystem> fs, ino_t _node_id,
               ktl::optional<PipeHandle> _fifo, FsNodeInfo info)
    : weak_handle(std::move(_weak_handle)),
      ops_(ktl::move(ops)),
      kernel_(std::move(kernel)),
      fs_(std::move(fs)),
      node_id(_node_id),
      fifo(std::move(_fifo)),
      info_(std::move(info)) {}

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
    return fit::ok(std::move(ktl::unique_ptr<OPathOps>(OPathOps::New())));
  }

  if (check_access) {
    if (flags.contains(OpenFlagsEnum::NOATIME)) {
      // self.check_o_noatime_allowed(current_task) ? ;
    }
    // self.check_access(current_task, mount, Access::from_open_flags(flags)) ? ;
  }

  auto [mode, rdev] = [&]() -> auto {
    auto info = this->info();
    return std::make_pair(info->mode, info->rdev);
  }();

  auto fmt_mode = (mode & FileMode::IFMT);
  if (fmt_mode == FileMode::IFCHR) {
    return fit::error(errno(ENOTSUP));
  } else if (fmt_mode == FileMode::IFBLK) {
    return fit::error(errno(ENOTSUP));
  } else if (fmt_mode == FileMode::IFIFO) {
    return fit::error(errno(ENOTSUP));
  } else if (fmt_mode == FileMode::IFSOCK) {
    // UNIX domain sockets can't be opened.
    return fit::error(errno(ENXIO));
  } else {
    return create_file_ops(current_task, flags);
  }
}

fit::result<Errno, FsNodeHandle> FsNode::lookup(const CurrentTask& current_task,
                                                const MountInfo& mount, const FsStr& name) const {
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
  } else {
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
    return ops_->mknod(/*&mut locked, */ *this, current_task, name, mode, dev, owner);
  }
}

fit::result<Errno, SymlinkTarget> FsNode::readlink(const CurrentTask& current_task) const {
  // TODO(qsr): Is there a permission check here?
  return ops_->readlink(*this, current_task);
}

fit::result<Errno, struct stat> FsNode::stat(const CurrentTask& current_task) const {
  auto result = refresh_info(current_task);
  if (result.is_error())
    return result.take_error();

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
      .st_size = static_cast<__kernel_long_t>(info.size),
  });
}

fit::result<Errno, FsNodeInfo> FsNode::refresh_info(const CurrentTask& current_task) const {
  return ops().refresh_info(*this, current_task, info_);
}

}  // namespace starnix
