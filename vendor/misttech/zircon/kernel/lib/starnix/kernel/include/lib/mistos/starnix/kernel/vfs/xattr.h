// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/sync/locks.h>
#include <lib/mistos/starnix/kernel/vfs/fs_args.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>

#include <vector>

#include <fbl/intrusive_hash_table.h>

namespace starnix {

struct MemoryXattrStorage {
  mutable StarnixMutex<fbl::HashTable<FsString, ktl::unique_ptr<fs_args::HashableFsString>>> xattrs;

  /// impl MemoryXattrStorage
  fit::result<Errno, FsString> get_xattr(const FsStr& name) const;

  fit::result<Errno> set_xattr(const FsStr& name, const FsStr& value, XattrOp op) const;

  fit::result<Errno> remove_xattr(const FsStr& name) const;

  fit::result<Errno, std::vector<FsString>> list_xattrs(const FsStr& name) const;

  /// impl Default
  static MemoryXattrStorage Default();
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_
