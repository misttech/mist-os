// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYMLINK_MODE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYMLINK_MODE_H_

#include <zircon/types.h>

namespace starnix {

// The `SymlinkMode` enum encodes how symlinks are followed during path traversal.
enum SymlinkMode : uint8_t {
  /// Follow a symlink at the end of a path resolution.
  Follow,

  /// Do not follow a symlink at the end of a path resolution.
  NoFollow,
};

// The maximum number of symlink traversals that can be made during path resolution.
uint8_t const MAX_SYMLINK_FOLLOWS = 40;

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYMLINK_MODE_H_
