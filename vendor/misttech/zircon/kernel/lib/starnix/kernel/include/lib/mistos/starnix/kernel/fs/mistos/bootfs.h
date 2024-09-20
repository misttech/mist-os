// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_BOOTFS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_BOOTFS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/file_system_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/xattr.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/range-map.h>
#include <lib/starnix/bootfs/vmo_storage_traits.h>
#include <lib/starnix_sync/locks.h>
#include <lib/zbitl/items/bootfs.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <object/handle.h>
#include <vm/vm_object.h>

namespace starnix {

using namespace starnix_sync;

using BootfsReader = zbitl::Bootfs<fbl::RefPtr<VmObject>>;
using BootfsView = zbitl::BootfsView<fbl::RefPtr<VmObject>>;

class BootFs : public FileSystemOps {
 public:
  static FileSystemHandle new_fs(const fbl::RefPtr<Kernel>& kernel, HandleOwner zbi_vmo);

  static fit::result<Errno, FileSystemHandle> new_fs_with_options(const fbl::RefPtr<Kernel>& kernel,
                                                                  HandleOwner zbi_vmo,
                                                                  FileSystemOptions options);

  fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                           const CurrentTask& current_task) final;

  const FsStr& name() final { return name_; }

 private:
  // Create a VMO and transfer the entry's data from Bootfs to it. This
  // operation will also decommit the transferred range in the Bootfs image.
  fit::result<Errno, fbl::RefPtr<MemoryObject>> create_vmo_from_bootfs(util::Range<uint64_t> range,
                                                                       uint64_t original_size);

 public:
  // C++
  BootFs(HandleOwner zbi_vmo);

  ~BootFs();

 private:
  const FsStr name_ = "bootfs";

  BootfsReader bootfs_reader_;
};

#if 0
class BootfsDirectory : public FsNodeOps {
 private:
  MemoryXattrStorage xattrs_;

  mutable StarnixMutex<uint32_t> child_count_;

 public:
  static BootfsDirectory* New(BootfsView view);

  /// impl FsNodeOps
  fs_node_impl_xattr_delegate(xattrs_);

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(
      /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task,
      OpenFlags flags) final;

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) final;

  fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node, const CurrentTask& current_task,
                                         const FsStr& name, FileMode mode, FsCred owner) final;

  fit::result<Errno, FsNodeHandle> mknod(const FsNode& node, const CurrentTask& current_task,
                                         const FsStr& name, FileMode mode, DeviceType dev,
                                         FsCred owner) final;

  fit::result<Errno, FsNodeHandle> create_symlink(const FsNode& node,
                                                  const CurrentTask& current_task,
                                                  const FsStr& name, const FsStr& target,
                                                  FsCred owner) final;

  fit::result<Errno, FsNodeHandle> create_tmpfile(const FsNode& node,
                                                  const CurrentTask& current_task, FileMode mode,
                                                  FsCred owner) final;

  fit::result<Errno> link(const FsNode& node, const CurrentTask& current_task, const FsStr& name,
                          const FsNodeHandle& child) final;

  fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task, const FsStr& name,
                            const FsNodeHandle& child) final;

 public:
  BootfsDirectory(BootfsView view) : view_(ktl::move(view)) {}

 private:
  BootfsView view_;
};

#endif

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_BOOTFS_H_
