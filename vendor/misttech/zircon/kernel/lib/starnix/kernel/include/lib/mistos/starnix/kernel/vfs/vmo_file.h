// Copyright 2024 Mist Tecnlogia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_VMO_FILE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_VMO_FILE_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/falloc.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>
#include <lib/mistos/starnix/kernel/vfs/xattr.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/seal_flags.h>

#include <fbl/ref_ptr.h>

class VmObjectDispatcher;

namespace starnix {

class VmoFileNode : public FsNodeOps {
 private:
  /// The memory that backs this file.
  KernelHandle<VmObjectDispatcher> vmo_;

  MemoryXattrStorage xattrs_ = MemoryXattrStorage::Default();

 public:
  /// impl VmoFileNode
  /// Create a new writable file node based on a blank VMO.
  static fit::result<Errno, VmoFileNode*> New();

  /// Create a new file node based on an existing VMO.
  /// Attempts to open the file for writing will fail unless [`vmo`] has both
  /// the `WRITE` and `RESIZE` rights.
  static VmoFileNode* from_vmo(fbl::RefPtr<VmObject> vmo);

  /// impl FsNodeOps
  fs_node_impl_not_dir;
  fs_node_impl_xattr_delegate(xattrs_);

  void initial_info(FsNodeInfo& info) final;

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(
      /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task,
      OpenFlags flags) final;

  fit::result<Errno> truncate(const FsNode& node, const CurrentTask& current_task,
                              uint64_t length) final;

  fit::result<Errno> allocate(const FsNode& node, const CurrentTask& current_task, FallocMode mode,
                              uint64_t offset, uint64_t length) final;

 private:
  VmoFileNode(KernelHandle<VmObjectDispatcher> vmo /*, MemoryXattrStorage xattrs*/)
      : vmo_(ktl::move(vmo)) /*, xattrs_(xattrs)*/ {}
};

#define fileops_impl_vmo(vmo)                                                                      \
  fileops_impl_seekable();                                                                         \
                                                                                                   \
  fit::result<Errno, size_t> read(const FileObject& file, const CurrentTask&, size_t offset,       \
                                  OutputBuffer* data) {                                            \
    return VmoFileObject::read(vmo, file, offset, data);                                           \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, size_t> write(const FileObject& file, const CurrentTask& current_task,        \
                                   size_t offset, InputBuffer* data) {                             \
    return VmoFileObject::write(vmo, file, current_task, offset, data);                            \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, fbl::RefPtr<VmObject>> get_vmo(const FileObject& file,                        \
                                                    const CurrentTask& current_task,               \
                                                    ktl::optional<size_t>, ProtectionFlags prot) { \
    return VmoFileObject::get_vmo(vmo, file, current_task, prot);                                  \
  }                                                                                                \
  using __fileops_impl_vmo_force_semicolon = int

class VmoFileObject : public FileOps {
 public:
  fbl::RefPtr<VmObjectDispatcher> vmo;

  /// impl VmoFileObject
  /// Create a file object based on a VMO.
  static VmoFileObject* New(fbl::RefPtr<VmObjectDispatcher> vmo);

  static fit::result<Errno, size_t> read(const fbl::RefPtr<VmObjectDispatcher>& vmo,
                                         const FileObject& file, size_t offset, OutputBuffer* data);

  static fit::result<Errno, size_t> write(const fbl::RefPtr<VmObjectDispatcher>& vmo,
                                          const FileObject& file, const CurrentTask& current_task,
                                          size_t offset, InputBuffer* data);

  static fit::result<Errno, fbl::RefPtr<VmObject>> get_vmo(
      const fbl::RefPtr<VmObjectDispatcher>& vmo, const FileObject&, const CurrentTask&,
      ProtectionFlags prot);

  /// impl FileOps
  fileops_impl_vmo(vmo);

  fit::result<Errno> readahead(const FileObject& file, const CurrentTask& current_task,
                               size_t offset, size_t length) final {
    // track_stub !(TODO("https://fxbug.dev/42082608"), "paged VMO readahead");
    return fit::ok();
  }

 private:
  VmoFileObject(fbl::RefPtr<VmObjectDispatcher> _vmo) : vmo(std::move(_vmo)) {}
};

fit::result<Errno, FileHandle> new_memfd(const CurrentTask& current_task, FsString name,
                                         SealFlags seals, OpenFlags flags);

/// Sets VMO size to `min_size` rounded to whole pages. Returns the new size of the VMO in bytes.
fit::result<Errno, size_t> update_vmo_file_size(const fbl::RefPtr<VmObjectDispatcher>& vmo,
                                                FsNodeInfo& node_info, size_t requested_size);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_VMO_FILE_H_
