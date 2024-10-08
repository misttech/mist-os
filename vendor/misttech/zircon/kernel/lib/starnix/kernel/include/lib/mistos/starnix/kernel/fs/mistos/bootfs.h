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

using BootfsReader = zbitl::Bootfs<fbl::RefPtr<VmObject>>;
using BootfsView = zbitl::BootfsView<fbl::RefPtr<VmObject>>;

class BootFs final : public FileSystemOps {
 public:
  static FileSystemHandle new_fs(const fbl::RefPtr<Kernel>& kernel,
                                 fbl::RefPtr<VmObjectDispatcher> vmo);

  static fit::result<Errno, FileSystemHandle> new_fs_with_options(
      const fbl::RefPtr<Kernel>& kernel, fbl::RefPtr<VmObjectDispatcher> vmo,
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
  explicit BootFs(const fbl::RefPtr<VmObjectDispatcher>& vmo);

  ~BootFs() final;

 private:
  const FsStr name_ = "bootfs";

  BootfsReader bootfs_reader_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_BOOTFS_H_
