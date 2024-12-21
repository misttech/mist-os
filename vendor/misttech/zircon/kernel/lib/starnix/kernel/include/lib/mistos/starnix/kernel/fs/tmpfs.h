// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_TMPFS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_TMPFS_H_

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
  /// impl TmpFs
  static FileSystemHandle new_fs(const fbl::RefPtr<Kernel>& kernel);

  static fit::result<Errno, FileSystemHandle> new_fs_with_options(const fbl::RefPtr<Kernel>& kernel,
                                                                  FileSystemOptions options);

  /// impl FileSystemOps for Arc<TmpFs>
  fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                           const CurrentTask& current_task) const final;

  fit::result<Errno> rename(const FileSystem& fs, const CurrentTask& current_task,
                            const FsNodeHandle& old_parent, const FsStr& old_name,
                            const FsNodeHandle& new_parent, const FsStr& new_name,
                            const FsNodeHandle& renamed,
                            ktl::optional<FsNodeHandle> replaced) const final;

  fit::result<Errno> exchange(const FileSystem& fs, const CurrentTask& current_task,
                              const FsNodeHandle& node1, const FsNodeHandle& parent1,
                              const FsStr& name1, const FsNodeHandle& node2,
                              const FsNodeHandle& parent2, const FsStr& name2) const final;

  const FsStr& name() const final;

 public:
  // C++
  ~TmpFs();

 private:
  const FsStr name_ = "tmpfs";
};

class TmpfsDirectory : public FsNodeOps {
 private:
  MemoryXattrStorage xattrs_;
  mutable starnix_sync::Mutex<uint32_t> child_count_;

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
  friend class TmpFs;
  TmpfsDirectory();
};

fit::result<Errno, FsNodeHandle> create_child_node(const CurrentTask& current_task,
                                                   const FsNode& parent, FileMode mode,
                                                   DeviceType dev, FsCred owner);

// Creates a new tmpfs filesystem with default options
fit::result<Errno, FileSystemHandle> tmp_fs(const CurrentTask& current_task,
                                            FileSystemOptions options);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_TMPFS_H_
