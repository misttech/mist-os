// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACENODE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACENODE_H_

#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/open_flags.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>
#include <ktl/variant.h>

namespace starnix {

class CurrentTask;
class DirEntry;
class FileObject;
class FileOps;
class Task;
class FsNode;
class LookupContext;
class SymlinkTarget;

using starnix_uapi::DeviceType;
using DirEntryHandle = fbl::RefPtr<DirEntry>;
using FileHandle = fbl::RefPtr<FileObject>;
using starnix_uapi::FileMode;
using starnix_uapi::FsCred;
using FsNodeHandle = fbl::RefPtr<FsNode>;
using starnix_uapi::Access;
using starnix_uapi::OpenFlags;

/// The path is reachable from the given root.
struct Reachable {
  FsString path;
};

/// The path is not reachable from the given root.
struct Unreachable {
  FsString path;
};

class PathWithReachability {
 public:
  using Variant = ktl::variant<Reachable, Unreachable>;

  static PathWithReachability Reachable(FsString path) {
    struct Reachable r = {.path = ktl::move(path)};
    return PathWithReachability(r);
  }
  static PathWithReachability Unreachable(FsString path) {
    struct Unreachable r = {.path = ktl::move(path)};
    return PathWithReachability(r);
  }

  // impl PathWithReachability
  FsString into_path() const {
    return ktl::visit(PathWithReachability::overloaded{
                          [&](const struct Reachable& r) { return r.path; },
                          [&](const struct Unreachable& u) { return u.path; },
                      },
                      variant_);
  }

 private:
  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  explicit PathWithReachability(Variant variant) : variant_(ktl::move(variant)) {}

  Variant variant_;
};

/// A node in a mount namespace.
///
/// This tree is a composite of the mount tree and the FsNode tree.
///
/// These nodes are used when traversing paths in a namespace in order to
/// present the client the directory structure that includes the mounted
/// filesystems.
class NamespaceNode {
 public:
  /// The mount where this namespace node is mounted.
  ///
  /// A given FsNode can be mounted in multiple places in a namespace. This
  /// field distinguishes between them.
  MountInfo mount_;

  /// The FsNode that corresponds to this namespace entry.
  DirEntryHandle entry_;

  // impl NamespaceNode
  static NamespaceNode New(MountHandle mount, DirEntryHandle dir_entry);

  /// Create a namespace node that is not mounted in a namespace.
  static NamespaceNode new_anonymous(DirEntryHandle dir_entry);

  /// Create a namespace node that is not mounted in a namespace and that refers to a node that
  /// is not rooted in a hierarchy and has no name.
  static NamespaceNode new_anonymous_unrooted(FsNodeHandle node);

  /// Create a FileObject corresponding to this namespace node.
  ///
  /// This function is the primary way of instantiating FileObjects. Each
  /// FileObject records the NamespaceNode that created it in order to
  /// remember its path in the Namespace.
  fit::result<Errno, FileHandle> open(const CurrentTask& current_task, OpenFlags flags,
                                      bool check_access) const;

  /// Create or open a node in the file system.
  ///
  /// Works for any type of node other than a symlink.
  ///
  /// Will return an existing node unless `flags` contains `OpenFlags::EXCL`.
  fit::result<Errno, NamespaceNode> open_create_node(const CurrentTask& current_task,
                                                     const FsStr& name, FileMode mode,
                                                     DeviceType dev, OpenFlags flags);

  /// Create a node in the file system.
  ///
  /// Works for any type of node other than a symlink.
  ///
  /// Does not return an existing node.
  fit::result<Errno, NamespaceNode> create_node(const CurrentTask& current_task, const FsStr& name,
                                                FileMode mode, DeviceType dev);

  /// Creates an anonymous file.
  ///
  /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
  ///
  /// Used by O_TMPFILE.
  fit::result<Errno, NamespaceNode> create_tmpfile(const CurrentTask& current_task, FileMode mode,
                                                   OpenFlags flags) const;

  /// Traverse down a parent-to-child link in the namespace.
  fit::result<Errno, NamespaceNode> lookup_child(const CurrentTask& current_task,
                                                 LookupContext& context,
                                                 const FsStr& basename) const;

  /// Traverse up a child-to-parent link in the namespace.
  ///
  /// This traversal matches the child-to-parent link in the underlying
  /// FsNode except at mountpoints, where the link switches from one
  /// filesystem to another.
  ktl::optional<NamespaceNode> parent() const;

  /// Returns the parent, but does not escape mounts i.e. returns None if this node
  /// is the root of a mount.
  ktl::optional<DirEntryHandle> parent_within_mount() const;

 private:
  /// If this is a mount point, return the root of the mount. Otherwise return self.
  NamespaceNode enter_mount() const;

  /// If this is the root of a mount, return the mount point. Otherwise return self.
  ///
  /// This is not exactly the same as parent(). If parent() is called on a root, it will escape
  /// the mount, but then return the parent of the mount point instead of the mount point.
  NamespaceNode escape_mount() const;

 public:
  /// If this node is the root of a mount, return it. Otherwise EINVAL.
  fit::result<Errno, MountHandle> mount_if_root() const;

 private:
  /// Returns the mountpoint at this location in the namespace.
  ///
  /// If this node is mounted in another node, this function returns the node
  /// at which this node is mounted. Otherwise, returns None.
  ktl::optional<NamespaceNode> mountpoint() const;

 public:
  /// The path from the task's root to this node.
  FsString path(const Task& task) const;

  /// The path from the root of the namespace to this node.
  FsString path_escaping_chroot() const;

  /// Returns the path to this node, accounting for a custom root.
  /// A task may have a custom root set by `chroot`.
  PathWithReachability path_from_root(ktl::optional<NamespaceNode>) const;

  NamespaceNode with_new_entry(DirEntryHandle entry) const;

  fit::result<Errno, SymlinkTarget> readlink(const CurrentTask& current_task) const;

  /// Check whether the node can be accessed in the current context with the specified access
  /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
  /// owner or is in the file's group.
  fit::result<Errno> check_access(const CurrentTask& current_task, Access access) const;

  fit::result<Errno> truncate(const CurrentTask& current_task, uint64_t length) const;

  // C++
  bool operator==(const NamespaceNode& other) const;

  ~NamespaceNode();
};

class SymlinkTarget {
 public:
  using Variant = ktl::variant<FsString, NamespaceNode>;

  static SymlinkTarget Path(const FsString& path);
  static SymlinkTarget Node(NamespaceNode node);

  ~SymlinkTarget();

  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<>:
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };

  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  Variant variant_;

 private:
  explicit SymlinkTarget(Variant variant);
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACENODE_H_
