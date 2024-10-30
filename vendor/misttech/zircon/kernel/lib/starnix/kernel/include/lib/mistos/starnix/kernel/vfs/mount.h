// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/vfs.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/variant.h>
#include <lockdep/guard.h>

namespace starnix {

class Mount;
class FileSystem;
class DirEntry;

using DirEntryHandle = fbl::RefPtr<DirEntry>;
using FileSystemHandle = fbl::RefPtr<FileSystem>;
using MountHandle = fbl::RefPtr<Mount>;
using starnix_uapi::MountFlags;

struct MountState {
 private:
  /// The namespace node that this mount is mounted on. This is a tuple instead of a
  /// NamespaceNode because the Mount pointer has to be weak because this is the pointer to the
  /// parent mount, the parent has a pointer to the children too, and making both strong would be
  /// a cycle.
  ktl::optional<ktl::pair<util::WeakPtr<Mount>, DirEntryHandle>> mountpoint;

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
  // peer_group_ : Option<(Arc<PeerGroup>, PtrKey<Mount>)>,
  /// The membership of this mount in a PeerGroup's downstream. Do not access
  /// directly. Instead use upstream(), take_from_upstream(), and set_upstream().
  // upstream_ : Option<(Weak<PeerGroup>, PtrKey<Mount>)>,

 private:
  // impl MountState

 private:
  // C++
  friend class Mount;
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
class Mount : public fbl::RefCountedUpgradeable<Mount> {
 private:
  DirEntryHandle root_;

  FileSystemHandle fs_;

  mutable starnix_sync::StarnixMutex<MountFlags> flags_;

  // A unique identifier for this mount reported in /proc/pid/mountinfo.
  uint64_t id_;

  /// A count of the number of active clients.
  // active_client_counter: MountClientMarker,

  mutable starnix_sync::RwLock<MountState> state_;
  // Mount used to contain a Weak<Namespace>. It no longer does because since the mount point
  // hash was moved from Namespace to Mount, nothing actually uses it. Now that
  // Namespace::clone_namespace() is implemented in terms of Mount::clone_mount_recursive, it
  // won't be trivial to add it back. I recommend turning the mountpoint field into an enum of
  // Mountpoint or Namespace, maybe called "parent", and then traverse up to the top of the tree
  // if you need to find a Mount's Namespace.

 public:
  /// impl Mount
  static MountHandle New(WhatToMount what, MountFlags flags);

  static MountHandle new_with_root(DirEntryHandle root, MountFlags flags);

  /// A namespace node referring to the root of the mount.
  NamespaceNode root();

  /// Returns true if there is a submount on top of `dir_entry`.
  // has_submount

  /// The NamespaceNode on which this Mount is mounted.
  ktl::optional<NamespaceNode> mountpoint() const;

  MountFlags flags() const;

 public:
  // C++
  ~Mount();

 private:
  friend struct NamespaceNode;

  Mount(uint64_t id, MountFlags flags, DirEntryHandle root, FileSystemHandle fs);
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_H_
