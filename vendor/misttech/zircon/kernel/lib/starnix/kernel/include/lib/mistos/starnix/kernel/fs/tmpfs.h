// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_TMPFS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_TMPFS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/file_system_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/xattr.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/starnix_sync/locks.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>

namespace starnix {

class Kernel;

class TmpFs : public FileSystemOps {
 public:
  static FileSystemHandle new_fs(const fbl::RefPtr<Kernel>& kernel);

  static fit::result<Errno, FileSystemHandle> new_fs_with_options(const fbl::RefPtr<Kernel>& kernel,
                                                                  FileSystemOptions options);

  fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                           const CurrentTask& current_task) final;

  const FsStr& name() final;

 public:
  // C++
  ~TmpFs();

 private:
  const FsStr name_ = "tmpfs";
};

class TmpfsDirectory : public FsNodeOps {
 private:
  MemoryXattrStorage xattrs_;
  mutable starnix_sync::StarnixMutex<uint32_t> child_count_;

 public:
  /// impl TmpfsDirectory
  static TmpfsDirectory* New();

  /// impl FsNodeOps
  fs_node_impl_xattr_delegate(xattrs_);

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const final;

  fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node, const CurrentTask& current_task,
                                         const FsStr& name, FileMode mode,
                                         FsCred owner) const final;

  fit::result<Errno, FsNodeHandle> mknod(const FsNode& node, const CurrentTask& current_task,
                                         const FsStr& name, FileMode mode, DeviceType dev,
                                         FsCred owner) const final;

  fit::result<Errno, FsNodeHandle> create_symlink(const FsNode& node,
                                                  const CurrentTask& current_task,
                                                  const FsStr& name, const FsStr& target,
                                                  FsCred owner) const final;

  fit::result<Errno, FsNodeHandle> create_tmpfile(const FsNode& node,
                                                  const CurrentTask& current_task, FileMode mode,
                                                  FsCred owner) const final;

  fit::result<Errno> link(const FsNode& node, const CurrentTask& current_task, const FsStr& name,
                          const FsNodeHandle& child) const final;

  fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task, const FsStr& name,
                            const FsNodeHandle& child) const final;

 private:
  TmpfsDirectory();
};

fit::result<Errno, FsNodeHandle> create_child_node(const CurrentTask& current_task,
                                                   const FsNode& parent, FileMode mode,
                                                   DeviceType dev, FsCred owner);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_TMPFS_H_
