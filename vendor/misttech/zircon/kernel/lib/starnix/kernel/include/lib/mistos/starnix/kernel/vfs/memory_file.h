// Copyright 2024 Mist Tecnlogia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MEMORY_FILE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MEMORY_FILE_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/vfs/falloc.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>
#include <lib/mistos/starnix/kernel/vfs/xattr.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/seal_flags.h>

#include <fbl/ref_ptr.h>

class VmObjectDispatcher;

namespace starnix {

class MemoryFileNode : public FsNodeOps {
 private:
  /// The memory that backs this file.
  fbl::RefPtr<MemoryObject> memory_;
  MemoryXattrStorage xattrs_ = MemoryXattrStorage::Default();

 public:
  /// impl VmoFileNode

  /// Create a new writable file node based on a blank VMO.
  static fit::result<Errno, MemoryFileNode*> New();

  /// Create a new file node based on an existing VMO.
  /// Attempts to open the file for writing will fail unless [`memory`] has both
  /// the `WRITE` and `RESIZE` rights.
  static MemoryFileNode* from_memory(fbl::RefPtr<MemoryObject> memory);

  /// impl FsNodeOps
  fs_node_impl_not_dir();
  fs_node_impl_xattr_delegate(xattrs_);

  void initial_info(FsNodeInfo& info) const final;

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const final;

  fit::result<Errno> truncate(const FsNode& node, const CurrentTask& current_task,
                              uint64_t length) const final;

  fit::result<Errno> allocate(const FsNode& node, const CurrentTask& current_task, FallocMode mode,
                              uint64_t offset, uint64_t length) const final;

 private:
  explicit MemoryFileNode(fbl::RefPtr<MemoryObject> memory) : memory_(ktl::move(memory)) {}
};

#define fileops_impl_memory(memory)                                                          \
  fileops_impl_seekable();                                                                   \
                                                                                             \
  fit::result<Errno, size_t> read(const FileObject& file, const CurrentTask&, size_t offset, \
                                  OutputBuffer* data) const final {                          \
    return MemoryFileObject::read(*(memory), file, offset, data);                            \
  }                                                                                          \
                                                                                             \
  fit::result<Errno, size_t> write(const FileObject& file, const CurrentTask& current_task,  \
                                   size_t offset, InputBuffer* data) const final {           \
    return MemoryFileObject::write(*(memory), file, current_task, offset, data);             \
  }                                                                                          \
                                                                                             \
  fit::result<Errno, fbl::RefPtr<MemoryObject>> get_memory(                                  \
      const FileObject& file, const CurrentTask& current_task, ktl::optional<size_t>,        \
      ProtectionFlags prot) const final {                                                    \
    return MemoryFileObject::get_memory(memory, file, current_task, prot);                   \
  }                                                                                          \
  using __fileops_impl_memory_force_semicolon = int

class MemoryFileObject : public FileOps {
 public:
  fbl::RefPtr<MemoryObject> memory_;

  /// impl VmoFileObject
  /// Create a file object based on a VMO.
  static MemoryFileObject* New(fbl::RefPtr<MemoryObject> memory);

  /// impl MemoryFileObject {
  static fit::result<Errno, size_t> read(const MemoryObject& memory, const FileObject& file,
                                         size_t offset, OutputBuffer* data);

  static fit::result<Errno, size_t> write(const MemoryObject& memory, const FileObject& file,
                                          const CurrentTask& current_task, size_t offset,
                                          InputBuffer* data);

  static fit::result<Errno, fbl::RefPtr<MemoryObject>> get_memory(
      const fbl::RefPtr<MemoryObject>& memory, const FileObject&, const CurrentTask&,
      ProtectionFlags prot);

  /// impl FileOps
  fileops_impl_memory(memory_);
  fileops_impl_noop_sync();

  fit::result<Errno> readahead(const FileObject& file, const CurrentTask& current_task,
                               size_t offset, size_t length) const final {
    // track_stub !(TODO("https://fxbug.dev/42082608"), "paged VMO readahead");
    return fit::ok();
  }

 private:
  explicit MemoryFileObject(fbl::RefPtr<MemoryObject> memory) : memory_(ktl::move(memory)) {}
};

fit::result<Errno, FileHandle> new_memfd(const CurrentTask& current_task, FsString name,
                                         SealFlags seals, OpenFlags flags);

/// Sets memory size to `min_size` rounded to whole pages. Returns the new size of the VMO in bytes.
fit::result<Errno, size_t> update_memory_file_size(const MemoryObject& memory,
                                                   FsNodeInfo& node_info, size_t requested_size);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MEMORY_FILE_H_
