// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECTORY_FILE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECTORY_FILE_H_

#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/util/btree_map.h>
#include <lib/starnix_sync/locks.h>

namespace starnix {

/// If the offset is less than 2, emits . and .. entries for the specified file.
///
/// The offset will always be at least 2 after this function returns successfully. It's often
/// necessary to subtract 2 from the offset in subsequent logic.
fit::result<Errno> emit_dotdot(const FileObject& file, DirentSink* sink);

// Directory file implementation that stores entries in memory
class MemoryDirectoryFile : public FileOps {
 private:
  /// The current position for readdir.
  ///
  /// When readdir is called multiple times, we need to return subsequent
  /// directory entries. This field records where the previous readdir
  /// stopped.
  ///
  /// The state is actually recorded twice: once in the offset for this
  /// FileObject and again here. Recovering the state from the offset is slow
  /// because we would need to iterate through the keys of the BTree. Having
  /// the FsString cached lets us search the keys of the BTree faster.
  ///
  /// The initial "." and ".." entries are not recorded here. They are
  /// represented only in the offset field in the FileObject.
  mutable starnix_sync::Mutex<util::Bound<FsString>> readdir_position_;

 public:
  // impl MemoryDirectoryFile
  static MemoryDirectoryFile* New();

  // impl FileOps for MemoryDirectoryFile
  fileops_impl_directory();
  fileops_impl_noop_sync();

  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,
                                 off_t current_offset, SeekTarget target) const override;

  fit::result<Errno> readdir(const FileObject& file, const CurrentTask& current_task,
                             DirentSink* sink) const override;

 private:
  MemoryDirectoryFile();
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECTORY_FILE_H_
