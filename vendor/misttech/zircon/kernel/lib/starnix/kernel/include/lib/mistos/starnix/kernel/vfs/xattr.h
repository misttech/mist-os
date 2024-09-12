// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_args.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/intrusive_hash_table.h>
#include <ktl/string_view.h>

#include <linux/xattr.h>

namespace starnix {

enum class XattrOp {
  Set,
  Create,
  Replace,
};

class XattrOpHelper {
 public:
  static uint32_t into_flags(XattrOp op) {
    switch (op) {
      case XattrOp::Set:
        return 0;
      case XattrOp::Create:
        return XATTR_CREATE;
      case XattrOp::Replace:
        return XATTR_REPLACE;
    }
    return 0;  // Default to 0 if op is not recognized
  }
};

struct MemoryXattrStorage {
  mutable starnix_sync::StarnixMutex<FsStringHashTable> xattrs;

  /// impl MemoryXattrStorage
  fit::result<Errno, FsString> get_xattr(const FsStr& name) const;

  fit::result<Errno> set_xattr(const FsStr& name, const FsStr& value, XattrOp op) const;

  fit::result<Errno> remove_xattr(const FsStr& name) const;

  fit::result<Errno, fbl::Vector<FsString>> list_xattrs(const FsStr& name) const;

  /// impl Default
  static MemoryXattrStorage Default();
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_XATTR_H_
