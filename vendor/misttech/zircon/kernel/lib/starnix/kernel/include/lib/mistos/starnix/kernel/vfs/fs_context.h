// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_CONTEXT_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_CONTEXT_H_

#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/ref_ptr.h>

namespace starnix {

class Namespace;
class FileSystem;

using FileSystemHandle = fbl::RefPtr<FileSystem>;
using FileMode = starnix_uapi::FileMode;

// The mutable state for an FsContext.
//
// This state is cloned in FsContext::fork.
struct FsContextState {
  /// The namespace tree for this FsContext.
  ///
  /// This field owns the mount table for this FsContext.
  fbl::RefPtr<Namespace> namespace_;

  /// The root of the namespace tree for this FsContext.
  ///
  /// Operations on the file system are typically either relative to this
  /// root or to the cwd().
  NamespaceNode root;

  /// The current working directory.
  NamespaceNode cwd;

  // See <https://man7.org/linux/man-pages/man2/umask.2.html>
  FileMode umask;
};

class FsContext : public fbl::RefCounted<FsContext> {
 private:
  /// The mutable state for this FsContext.
  mutable starnix_sync::RwLock<FsContextState> state_;

 public:
  /// impl FsContext

  /// Create an FsContext for the given namespace.
  ///
  /// The root and cwd of the FsContext are initialized to the root of the
  /// namespace.
  static fbl::RefPtr<FsContext> New(fbl::RefPtr<Namespace> _namespace);

  fbl::RefPtr<FsContext> fork() const;

  /// Returns a reference to the current working directory.
  NamespaceNode cwd() const;

  /// Returns the root.
  NamespaceNode root() const;

  FileMode umask() const;

  FileMode apply_umask(FileMode mode) const;

  FileMode set_umask(FileMode umask) const;

  ~FsContext();

 private:
  explicit FsContext(FsContextState state);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_CONTEXT_H_
