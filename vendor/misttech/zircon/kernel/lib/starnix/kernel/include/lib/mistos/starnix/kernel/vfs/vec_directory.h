// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_VEC_DIRECTORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_VEC_DIRECTORY_H_

#include <lib/mistos/starnix/kernel/vfs/directory_file.h>
#include <lib/mistos/starnix/kernel/vfs/dirent_sink.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>

#include <ktl/unique_ptr.h>

namespace starnix {

/// A directory entry used for [`VecDirectory`].
struct VecDirectoryEntry {
  // The type of the directory entry (directory, regular, socket, etc).
  DirectoryEntryType entry_type;

  // The name of the directory entry.
  FsString name;

  // Optional inode associated with the entry. If nullopt, the entry will be auto-assigned one.
  ktl::optional<ino_t> inode;
};

class VecDirectory : public FileOps {
 public:
  static ktl::unique_ptr<FileOps> new_file(fbl::Vector<VecDirectoryEntry> entries) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) VecDirectory(ktl::move(entries));
    ZX_ASSERT(ac.check());
    return ktl::unique_ptr<FileOps>(ptr);
  }

  // impl FileOps for VecDirectory
  fileops_impl_directory();
  fileops_impl_noop_sync();

  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,
                                 off_t current_offset, SeekTarget target) override {
    return unbounded_seek(current_offset, target);
  }

  fit::result<Errno> readdir(const FileObject& file, const CurrentTask& current_task,
                             DirentSink* sink) override {
    _EP(emit_dotdot(file, sink));

    // Skip through the entries until the current offset is reached.
    // Subtract 2 from the offset to account for `.` and `..`.
    size_t start_idx = sink->offset() - 2;
    for (size_t i = start_idx; i < entries_.size(); i++) {
      const auto& entry = entries_[i];
      // Assign an inode if one wasn't set.
      ino_t inode = entry.inode.value_or(file.fs_->next_node_id());
      _EP(sink->add(inode, sink->offset() + 1, entry.entry_type, entry.name.c_str()));
    }
    return fit::ok();
  }

 private:
  VecDirectory(fbl::Vector<VecDirectoryEntry> entries) : entries_(ktl::move(entries)) {}

  fbl::Vector<VecDirectoryEntry> entries_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_VEC_DIRECTORY_H_
