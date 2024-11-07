// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECTORY_FILE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECTORY_FILE_H_

#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/starnix_sync/locks.h>

namespace starnix {

// Helper function to emit "." and ".." directory entries
// Returns ZX_OK on success, error code on failure
fit::result<Errno> emit_dotdot(const FileObject& file, DirentSink* sink);

// Directory file implementation that stores entries in memory
class MemoryDirectoryFile : public FileOps {
 public:
  static ktl::unique_ptr<FileOps> new_file() {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) MemoryDirectoryFile();
    ZX_ASSERT(ac.check());
    return ktl::unique_ptr<FileOps>(ptr);
  }

  // impl FileOps for MemoryDirectoryFile
  fileops_impl_directory();
  fileops_impl_noop_sync();

  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,
                                 off_t current_offset, SeekTarget target) const override {
    auto new_offset = unbounded_seek(current_offset, target);
    if (!new_offset.is_ok()) {
      return new_offset;
    }

    // Nothing to do if offset hasn't changed
    if (current_offset == new_offset.value()) {
      return new_offset;
    }

    auto guard = readdir_position_.Lock();

    // We use 0 and 1 for "." and ".."
    if (new_offset.value() <= 2) {
      //*guard = ktl::nullopt;
    } else {
      // TODO: Implement getting children and updating position
      // This requires additional filesystem infrastructure
    }

    return new_offset;
  }

 private:
  MemoryDirectoryFile() = default;

  // Current position for readdir operations
  // nullopt represents an unbounded/start position
  mutable starnix_sync::Mutex<FsString> readdir_position_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECTORY_FILE_H_
