// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_args.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/intrusive_hash_table.h>
#include <ktl/string_view.h>

namespace starnix {

class MemoryXattrStorage : public XattrStorage {
 private:
  mutable starnix_sync::StarnixMutex<FsStringHashTable> xattrs;

 public:
  /// impl XattrStorage
  fit::result<Errno, FsString> get_xattr(const FsStr& name) const override;

  fit::result<Errno> set_xattr(const FsStr& name, const FsStr& value, XattrOp op) const override;

  fit::result<Errno> remove_xattr(const FsStr& name) const override;

  fit::result<Errno, fbl::Vector<FsString>> list_xattrs(const FsStr& name) const override;

  /// impl Default
  static MemoryXattrStorage Default();

 private:
  MemoryXattrStorage() = default;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_
