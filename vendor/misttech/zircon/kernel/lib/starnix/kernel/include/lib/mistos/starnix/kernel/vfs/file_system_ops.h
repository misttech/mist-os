// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_OPS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_OPS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>

#include <asm/statfs.h>

namespace starnix {

class FileSystem;
class CurrentTask;
class FsNode;
using FsNodeHandle = fbl::RefPtr<FsNode>;

class FileSystemOps {
 public:
  virtual ~FileSystemOps() = default;

  /// Return information about this filesystem.
  ///
  /// A typical implementation looks like this:
  /// ```
  /// Ok(statfs::default(FILE_SYSTEM_MAGIC))
  /// ```
  /// or, if the filesystem wants to customize fields:
  /// ```
  /// Ok(statfs {
  ///     f_blocks: self.blocks,
  ///     ..statfs::default(FILE_SYSTEM_MAGIC)
  /// })
  /// ```
  virtual fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                                   const CurrentTask& current_task) = 0;

  virtual const FsStr& name() = 0;

  /// Whether this file system generates its own node IDs.
  virtual bool generate_node_ids() { return false; }

  /// Rename the given node.
  ///
  /// The node to be renamed is passed as "renamed". It currently has
  /// old_name in old_parent. After the rename operation, it should have
  /// new_name in new_parent.
  ///
  /// If new_parent already has a child named new_name, that node is passed as
  /// "replaced". In that case, both "renamed" and "replaced" will be
  /// directories and the rename operation should succeed only if "replaced"
  /// is empty. The VFS will check that there are no children of "replaced" in
  /// the DirEntry cache, but the implementation of this function is
  /// responsible for checking that there are no children of replaced that are
  /// known only to the file system implementation (e.g., present on-disk but
  /// not in the DirEntry cache).
  virtual fit::result<Errno> rename(const FileSystem& fs, const CurrentTask& current_task,
                                    const FsNodeHandle& old_parent, const FsStr& old_name,
                                    const FsNodeHandle& new_parent, const FsStr& new_name,
                                    const FsNodeHandle& renamed, const FsNodeHandle* replaced) {
    return fit::error(errno(EROFS));
  }

  virtual fit::result<Errno> exchange(const FileSystem& fs, const CurrentTask& current_task,
                                      const FsNodeHandle& node1, const FsNodeHandle& parent1,
                                      const FsStr& name1, const FsNodeHandle& node2,
                                      const FsNodeHandle& parent2, const FsStr& name2) {
    return fit::error(errno(EINVAL));
  }

  /// Called when the filesystem is unmounted.
  virtual void unmount() {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_OPS_H_
