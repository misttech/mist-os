// Copyright 2024 Mist Tecnlogia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_ANON_NODE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_ANON_NODE_H_

#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/vfs.h>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

#include <linux/magic.h>

namespace starnix {

class Anon : public FsNodeOps {
 public:
  static FileHandle new_file_extended(const CurrentTask& current_task, ktl::unique_ptr<FileOps> ops,
                                      OpenFlags flags, std::function<FsNodeInfo(ino_t)> info);

  static FileHandle new_file(const CurrentTask& current_task, ktl::unique_ptr<FileOps> ops,
                             OpenFlags flags);

  fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task, const FsStr& name,
                            const FsNodeHandle& child) final {
    return fit::error(errno(ENOTDIR));
  }

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(
      /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task,
      OpenFlags flags) final {
    return fit::error(errno(ENOSYS));
  }
};

class AnonFs : public FileSystemOps {
 public:
  fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                           const CurrentTask& current_task) final {
    return fit::ok(default_statfs(ANON_INODE_FS_MAGIC));
  }

  const FsStr& name() final { return name_; }

 private:
  const FsStr name_ = "anon";
};

FileSystemHandle anon_fs(const fbl::RefPtr<Kernel>& kernel);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_ANON_NODE_H_
