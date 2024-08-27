// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/vfs.h>

#include <optional>
#include <utility>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/variant.h>
#include <lockdep/guard.h>

namespace starnix {

using namespace starnix_uapi;

/// Public representation of the mount options.
struct MountInfo {
  /// `MountInfo` for a element that is not tied to a given mount. Mount flags will be considered
  /// empty.
  static MountInfo detached() { return {std::nullopt}; }

  /// The mount flags of the represented mount.
  MountFlags flags();

  /// Checks whether this `MountInfo` represents a writable file system mounted.
  fit::result<Errno> check_readonly_filesystem();

  ktl::optional<MountHandle> handle;
};

/// A node in a mount namespace.
///
/// This tree is a composite of the mount tree and the FsNode tree.
///
/// These nodes are used when traversing paths in a namespace in order to
/// present the client the directory structure that includes the mounted
/// filesystems.
struct NamespaceNode {
  /// The mount where this namespace node is mounted.
  ///
  /// A given FsNode can be mounted in multiple places in a namespace. This
  /// field distinguishes between them.
  MountInfo mount;

  /// The FsNode that corresponds to this namespace entry.
  DirEntryHandle entry;

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

  /// If this is a mount point, return the root of the mount. Otherwise return self.
  NamespaceNode enter_mount() const;

  NamespaceNode with_new_entry(DirEntryHandle _entry) const;

  fit::result<Errno, SymlinkTarget> readlink(const CurrentTask& current_task) const;

  /// Check whether the node can be accessed in the current context with the specified access
  /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
  /// owner or is in the file's group.
  fit::result<Errno> check_access(const CurrentTask& current_task, Access access) const;

  fit::result<Errno> truncate(const CurrentTask& current_task, uint64_t length) const;

  bool operator==(const NamespaceNode& other) const {
    return (mount.handle == other.mount.handle) && (entry == other.entry);
  }
};

struct MountState {
  /// The namespace node that this mount is mounted on. This is a tuple instead of a
  /// NamespaceNode because the Mount pointer has to be weak because this is the pointer to the
  /// parent mount, the parent has a pointer to the children too, and making both strong would be
  /// a cycle.
  // mountpoint: Option<(Weak<Mount>, DirEntryHandle)>,

  // The keys of this map are always descendants of this mount's root.
  //
  // Each directory entry can only have one mount attached. Mount shadowing works by using the
  // root of the inner mount as a mountpoint. For example, if filesystem A is mounted at /foo,
  // mounting filesystem B on /foo will create the mount as a child of the A mount, attached to
  // A's root, instead of the root mount.
  // submounts: HashMap<ArcKey<DirEntry>, MountHandle>,

  /// The membership of this mount in its peer group. Do not access directly. Instead use
  /// peer_group(), take_from_peer_group(), and set_peer_group().
  // TODO(tbodt): Refactor the links into, some kind of extra struct or something? This is hard
  // because setting this field requires the Arc<Mount>.
  // peer_group_: Option<(Arc<PeerGroup>, PtrKey<Mount>)>,
  /// The membership of this mount in a PeerGroup's downstream. Do not access directly. Instead
  /// use upstream(), take_from_upstream(), and set_upstream().
  // upstream_: Option<(Weak<PeerGroup>, PtrKey<Mount>)>,
};

enum class WhatToMountEnum {
  Fs,
  Bind,
};

struct WhatToMount {
  WhatToMountEnum type;
  ktl::variant<FileSystemHandle, NamespaceNode> what;
};

/// An instance of a filesystem mounted in a namespace.
///
/// At a mount, path traversal switches from one filesystem to another.
/// The client sees a composed directory structure that glues together the
/// directories from the underlying FsNodes from those filesystems.
///
/// The mounts in a namespace form a mount tree, with `mountpoint` pointing to the parent and
/// `submounts` pointing to the children.
class Mount : public fbl::RefCounted<Mount> {
 public:
  static MountHandle New(WhatToMount what, MountFlags flags);
  static MountHandle new_with_root(DirEntryHandle root, MountFlags flags);

  // A namespace node referring to the root of the mount.
  NamespaceNode root();

  MountFlags flags() {
    Guard<Mutex> lock(&mount_flags_lock_);
    return flags_;
  }

 private:
  Mount(uint64_t id, MountFlags flags, DirEntryHandle root, FileSystemHandle fs);

  DirEntryHandle root_;

#ifdef __clang__
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunused-private-field"
#endif

  DECLARE_MUTEX(Mount) mount_flags_lock_;
  MountFlags flags_ __TA_GUARDED(mount_flags_lock_);

  FileSystemHandle fs_;

  // A unique identifier for this mount reported in /proc/pid/mountinfo.
  uint64_t id_;

  // Lock ordering: mount -> submount
  DECLARE_MUTEX(Mount) mount_state_lock_;
  MountState state_ __TA_GUARDED(mount_state_lock_);
  // Mount used to contain a Weak<Namespace>. It no longer does because since the mount point
  // hash was moved from Namespace to Mount, nothing actually uses it. Now that
  // Namespace::clone_namespace() is implemented in terms of Mount::clone_mount_recursive, it
  // won't be trivial to add it back. I recommend turning the mountpoint field into an enum of
  // Mountpoint or Namespace, maybe called "parent", and then traverse up to the top of the tree
  // if you need to find a Mount's Namespace.

#ifdef __clang__
#pragma clang diagnostic pop
#endif
};

// A mount namespace.
//
// The namespace records at which entries filesystems are mounted.
class Namespace : public fbl::RefCounted<Namespace> {
 public:
  static fbl::RefPtr<Namespace> New(FileSystemHandle fs);

  NamespaceNode root() { return root_mount_->root(); }

 private:
  Namespace(MountHandle root_mount, uint64_t id) : root_mount_(std::move(root_mount)), id_(id) {}

  MountHandle root_mount_;

#ifdef __clang__
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunused-private-field"
#endif

  // Unique ID of this namespace.
  uint64_t id_;

#ifdef __clang__
#pragma clang diagnostic pop
#endif
};

// The `SymlinkMode` enum encodes how symlinks are followed during path traversal.
enum SymlinkMode {
  /// Follow a symlink at the end of a path resolution.
  Follow,

  /// Do not follow a symlink at the end of a path resolution.
  NoFollow,
};

// The maximum number of symlink traversals that can be made during path resolution.
uint8_t const MAX_SYMLINK_FOLLOWS = 40;

/// Used to specify base directory in `LookupContext` for lookups originating in the `openat2`
/// syscall with either `RESOLVE_BENEATH` or `RESOLVE_IN_ROOT` flag.
enum ResolveBaseType {
  None,

  // The lookup is not allowed to traverse any node that's not beneath the specified node.
  Beneath,

  // The lookup should be handled as if the root specified node is the file-system root.
  InRoot,
};

struct ResolveBase {
  ResolveBaseType type;
  NamespaceNode node;
};

/// The context passed during namespace lookups.
///
/// Namespace lookups need to mutate a shared context in order to correctly
/// count the number of remaining symlink traversals.
struct LookupContext {
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
  //
  //
  static LookupContext New(SymlinkMode _symlink_mode);

  LookupContext with(SymlinkMode _symlink_mode);

  void update_for_path(const FsStr& path);

  /// impl Default
  //
  //
  static LookupContext Default();
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_H_
