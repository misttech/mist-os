// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_NODE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_NODE_H_

#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/unmount_flags.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/optional.h>
#include <ktl/variant.h>

namespace starnix {

using starnix_uapi::Access;
using starnix_uapi::AccessCheck;
using starnix_uapi::DeviceType;
using starnix_uapi::Errno;
using starnix_uapi::FileMode;
using starnix_uapi::FsCred;
using starnix_uapi::OpenFlags;
using starnix_uapi::UnmountFlags;
using starnix_uapi::UserAndOrGroupId;

class CurrentTask;
class DirEntry;
class FileObject;
class FileOps;
class Task;
class FsNode;
class LookupContext;
class SymlinkTarget;
class WhatToMount;

using DirEntryHandle = fbl::RefPtr<DirEntry>;
using FileHandle = fbl::RefPtr<FileObject>;
using FsNodeHandle = fbl::RefPtr<FsNode>;

/// The path is reachable from the given root.
struct Reachable {
  FsString path;
};

/// The path is not reachable from the given root.
struct Unreachable {
  FsString path;
};

enum class CheckAccessReason : uint8_t { Access, Chdir, Chroot, InternalPermissionChecks };

class PathWithReachability {
 public:
  using Variant = ktl::variant<Reachable, Unreachable>;
  Variant variant_;

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

  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

 private:
  explicit PathWithReachability(Variant variant) : variant_(ktl::move(variant)) {}
};

class ActiveNamespaceNode;

enum class UnlinkKind : uint8_t {
  /// Unlink a directory.
  Directory,

  /// Unlink a non-directory.
  NonDirectory
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
                                      AccessCheck access_check) const;

  /// Create or open a node in the file system.
  ///
  /// Works for any type of node other than a symlink.
  ///
  /// Will return an existing node unless `flags` contains `OpenFlags::EXCL`.
  fit::result<Errno, NamespaceNode> open_create_node(const CurrentTask& current_task,
                                                     const FsStr& name, FileMode mode,
                                                     DeviceType dev, OpenFlags flags) const;

  /// Convert this namespace node into an active namespace node.
  ActiveNamespaceNode into_active() const;

  /// Create a node in the file system.
  ///
  /// Works for any type of node other than a symlink.
  ///
  /// Does not return an existing node.
  fit::result<Errno, NamespaceNode> create_node(const CurrentTask& current_task, const FsStr& name,
                                                FileMode mode, DeviceType dev) const;

  /// Create a symlink in the file system.
  ///
  /// To create another type of node, use `create_node`.
  fit::result<Errno, NamespaceNode> create_symlink(const CurrentTask& current_task,
                                                   const FsStr& name, const FsStr& target) const;

  /// Creates an anonymous file.
  ///
  /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
  ///
  /// Used by O_TMPFILE.
  fit::result<Errno, NamespaceNode> create_tmpfile(const CurrentTask& current_task, FileMode mode,
                                                   OpenFlags flags) const;

  fit::result<Errno, NamespaceNode> link(const CurrentTask& current_task, const FsStr& name,
                                         const FsNodeHandle& child) const;

  fit::result<Errno> unlink(const CurrentTask& current_task, const FsStr& name, UnlinkKind kind,
                            bool must_be_directory) const;

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

  fit::result<Errno> mount(WhatToMount what, MountFlags flags) const;

  /// If this is the root of a filesystem, unmount. Otherwise return EINVAL.
  fit::result<Errno> unmount(UnmountFlags flags) const;

  static fit::result<Errno> rename(const CurrentTask& current_task, const NamespaceNode& old_parent,
                                   const FsStr& old_name, const NamespaceNode& new_parent,
                                   const FsStr& new_name, RenameFlags flags);

  NamespaceNode with_new_entry(DirEntryHandle entry) const;

  fit::result<Errno, UserAndOrGroupId> suid_and_sgid(const CurrentTask& current_task) const;

  fit::result<Errno, SymlinkTarget> readlink(const CurrentTask& current_task) const;

  /// Check whether the node can be accessed in the current context with the specified access
  /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
  /// owner or is in the file's group.
  fit::result<Errno> check_access(const CurrentTask& current_task, Access access,
                                  CheckAccessReason reason) const;

  fit::result<Errno> truncate(const CurrentTask& current_task, uint64_t length) const;

  // impl fmt::Debug for NamespaceNode
  mtl::BString debug() const;

  // C++
  // NamespaceNode(const NamespaceNode& other);
  NamespaceNode& operator=(const NamespaceNode& other);
  bool operator==(const NamespaceNode& other) const;

  ~NamespaceNode();
};

/// An empty struct that we use to track the number of active clients for a mount.
///
/// Each active client takes a reference to this object. The unmount operation fails
/// if there are any active clients of the mount.
struct Marker : public fbl::RefCounted<Marker> {};
using MountClientMarker = fbl::RefPtr<Marker>;

/// A namespace node that keeps the underlying mount busy.
class ActiveNamespaceNode {
 private:
  /// The underlying namespace node.
  NamespaceNode name_;

  /// Adds a reference to the mount client marker to prevent the mount from
  /// being removed while the NamespaceNode is active. Is None iff mount is
  /// None.
  ktl::optional<MountClientMarker> marker_;

 public:
  /// Create an ActiveNamespaceNode from a NamespaceNode
  static ActiveNamespaceNode New(NamespaceNode name);

  /// Get the underlying passive NamespaceNode
  NamespaceNode to_passive() const;

  ~ActiveNamespaceNode();

  // Dereference operators to access the underlying NamespaceNode
  const NamespaceNode& operator*() const;
  const NamespaceNode* operator->() const;
  NamespaceNode& operator*();
  NamespaceNode* operator->();

  bool operator==(const ActiveNamespaceNode& other) const;

 private:
  explicit ActiveNamespaceNode(NamespaceNode name, ktl::optional<MountClientMarker> marker);
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

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_NODE_H_
