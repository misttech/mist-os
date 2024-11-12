// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_LOOKUP_CONTEXT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_LOOKUP_CONTEXT_H_

#include <lib/mistos/starnix/kernel/vfs/resolve_base.h>
#include <lib/mistos/starnix/kernel/vfs/symlink_mode.h>
#include <lib/mistos/starnix_uapi/vfs.h>

namespace starnix {

using starnix_uapi::ResolveFlags;

/// The context passed during namespace lookups.
///
/// Namespace lookups need to mutate a shared context in order to correctly
/// count the number of remaining symlink traversals.
class LookupContext {
 public:
  /// The SymlinkMode for the lookup.
  ///
  /// As the lookup proceeds, the follow count is decremented each time the
  /// lookup traverses a symlink.
  SymlinkMode symlink_mode;

  /// The number of symlinks remaining the follow.
  ///
  /// Each time path resolution calls readlink, this value is decremented.
  uint8_t remaining_follows;

  /// Whether the result of the lookup must be a directory.
  ///
  /// For example, if the path ends with a `/` or if userspace passes
  /// O_DIRECTORY. This flag can be set to true if the lookup encounters a
  /// symlink that ends with a `/`.
  bool must_be_directory;

  /// Resolve flags passed to `openat2`. Empty if the lookup originated in any other syscall.
  ResolveFlags resolve_flags;

  /// Base directory for the lookup. Set only when either `RESOLVE_BENEATH` or `RESOLVE_IN_ROOT`
  /// is passed to `openat2`.
  ResolveBase resolve_base;

  /// impl LookupContext
  static LookupContext New(SymlinkMode _symlink_mode);

  LookupContext with(SymlinkMode _symlink_mode);

  void update_for_path(const FsStr& path);

  /// impl Default
  static LookupContext Default();
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_LOOKUP_CONTEXT_H_
