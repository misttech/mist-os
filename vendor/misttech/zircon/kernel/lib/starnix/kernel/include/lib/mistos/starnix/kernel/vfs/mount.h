// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_H_

#include <lib/fit/result.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/vfs.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/intrusive_hash_table.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/variant.h>
#include <lockdep/guard.h>

namespace starnix {

using starnix_uapi::Errno;
using starnix_uapi::MountFlags;

class Mount;
class FileSystem;
class DirEntry;

using DirEntryHandle = fbl::RefPtr<DirEntry>;
using FileSystemHandle = fbl::RefPtr<FileSystem>;
using MountHandle = fbl::RefPtr<Mount>;

// A RAII object that unregisters a mount when dropped.
class Submount {
 private:
  DirEntryHandle dir_;
  MountHandle mount_;

 public:
  Submount(DirEntryHandle dir, MountHandle mount);
  ~Submount();

  DirEntryHandle dir() const;
  MountHandle mount() const;

  static size_t GetHash(const Submount& key);

  bool operator==(const Submount& other) const;

 private:
  friend class Mount;
  friend class MountState;
};

class SubmountState : public fbl::SinglyLinkedListable<ktl::unique_ptr<SubmountState>> {
 public:
  explicit SubmountState(const Submount& submount);

  // hashtable support
  Submount GetKey() const;
  static size_t GetHash(const Submount& submount);

  // Allow dereferencing of the underlying key
  const Submount& operator*() const { return submount_; }
  Submount& operator*() { return submount_; }
  const Submount* operator->() const { return &submount_; }
  Submount* operator->() { return &submount_; }

 private:
  SubmountState(const SubmountState&) = delete;
  SubmountState(SubmountState&&) = delete;
  SubmountState& operator=(const SubmountState&) = delete;
  SubmountState& operator=(SubmountState&&) = delete;

  Submount submount_;
};

class MountHandleVector : public fbl::Vector<MountHandle>,
                          public fbl::SinglyLinkedListable<ktl::unique_ptr<MountHandleVector>> {
 public:
  explicit MountHandleVector(mtl::WeakPtr<DirEntry>);

  mtl::WeakPtr<DirEntry> GetKey() const;
  static size_t GetHash(const mtl::WeakPtr<DirEntry>& key);

 private:
  mtl::WeakPtr<DirEntry> key_;
};

// Tracks all mounts, keyed by mount point.
class Mounts {
 private:
  // Map of mount points to their associated mounts
  mutable starnix_sync::Mutex<
      fbl::HashTable<mtl::WeakPtr<DirEntry>, ktl::unique_ptr<MountHandleVector>>>
      mounts_;

 public:
  Mounts() = default;

  // Registers the mount in the global mounts map
  ktl::unique_ptr<SubmountState> register_mount(const DirEntryHandle& dir_entry,
                                                MountHandle mount) const;

  // Unregisters the mount. Called by Submount::drop
  void unregister_mount(const DirEntryHandle& dir_entry, const MountHandle& mount) const;

  // Unmounts all mounts associated with dir_entry. Called when dir_entry is unlinked.
  void unmount(const DirEntry& dir_entry) const;
};

struct PeerGroupState {
  // fbl::HashSet<mtl::WeakPtr<Mount>> mounts;
  // fbl::HashSet<mtl::WeakPtr<Mount>> downstream;
};

// A group of mounts. Setting MS_SHARED on a mount puts it in its own peer group. Any bind mounts
// of a mount in the group are also added to the group. A mount created in any mount in a peer
// group will be automatically propagated (recreated) in every other mount in the group.
class PeerGroup : public fbl::RefCountedUpgradeable<PeerGroup> {
 private:
  // uint64_t id_ = 0;
  mutable starnix_sync::RwLock<PeerGroupState> state_;

 public:
  PeerGroup() = default;

 private:
  friend class Mount;
};

class MountState {
 private:
  /// The namespace node that this mount is mounted on. This is a tuple instead of a
  /// NamespaceNode because the Mount pointer has to be weak because this is the pointer to the
  /// parent mount, the parent has a pointer to the children too, and making both strong would be
  /// a cycle.
  ktl::optional<ktl::pair<mtl::WeakPtr<Mount>, DirEntryHandle>> mountpoint_;

  // The set is keyed by the mountpoints which are always descendants of this mount's root.
  // Conceptually, the set is more akin to a map: `DirEntry -> MountHandle`, but we use a set
  // instead because `Submount` has a drop implementation that needs both the key and value.
  //
  // Each directory entry can only have one mount attached. Mount shadowing works by using the
  // root of the inner mount as a mountpoint. For example, if filesystem A is mounted at /foo,
  // mounting filesystem B on /foo will create the mount as a child of the A mount, attached to
  // A's root, instead of the root mount.
  using HashSet = fbl::HashTable<Submount, ktl::unique_ptr<SubmountState>>;
  HashSet submounts_;

  /// The membership of this mount in its peer group. Do not access directly. Instead use
  /// peer_group(), take_from_peer_group(), and set_peer_group().
  // TODO(tbodt): Refactor the links into, some kind of extra struct or something? This is hard
  // because setting this field requires the Arc<Mount>.
  // peer_group_ : Option<(Arc<PeerGroup>, PtrKey<Mount>)>,
  /// The membership of this mount in a PeerGroup's downstream. Do not access
  /// directly. Instead use upstream(), take_from_upstream(), and set_upstream().
  // upstream_ : Option<(Weak<PeerGroup>, PtrKey<Mount>)>,

  /// impl MountState<Base = Mount, BaseType = Arc<Mount>>

  /// Add a child mount *without propagating it to the peer group*. For internal use only.
  void add_submount_internal(const DirEntryHandle& dir, MountHandle mount);

  // C++
  friend class Mount;
  friend class NamespaceNode;

  Mount* base_ = nullptr;
};

class WhatToMount {
 public:
  using Variant = ktl::variant<FileSystemHandle, NamespaceNode>;

  static WhatToMount Fs(FileSystemHandle fs);
  static WhatToMount Bind(NamespaceNode node);

  // C++
  ~WhatToMount();

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

  friend class Mount;
  explicit WhatToMount(Variant what);

  Variant what_;
};

/// An instance of a filesystem mounted in a namespace.
///
/// At a mount, path traversal switches from one filesystem to another.
/// The client sees a composed directory structure that glues together the
/// directories from the underlying FsNodes from those filesystems.
///
/// The mounts in a namespace form a mount tree, with `mountpoint` pointing to the parent and
/// `submounts` pointing to the children.
class Mount : public fbl::WAVLTreeContainable<fbl::RefPtr<Mount>>,
              public fbl::RefCountedUpgradeable<Mount> {
 private:
  DirEntryHandle root_;
  mutable starnix_sync::Mutex<MountFlags> flags_;
  FileSystemHandle fs_;

  // A unique identifier for this mount reported in /proc/pid/mountinfo.
  uint64_t id_;

  /// A count of the number of active clients.
  MountClientMarker active_client_counter_;

  // Lock ordering: mount -> submount
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

  static MountHandle new_with_root(const DirEntryHandle& root, MountFlags flags);

  /// A namespace node referring to the root of the mount.
  NamespaceNode root();

  /// Returns true if there is a submount on top of `dir_entry`.
  bool has_submount(const DirEntryHandle& dir_entry) const;

 private:
  /// The NamespaceNode on which this Mount is mounted.
  ktl::optional<NamespaceNode> mountpoint() const;

  /// Create the specified mount as a child. Also propagate it to the mount's peer group.
  void create_submount(const DirEntryHandle& dir, WhatToMount what, MountFlags flags) const;

  /// Create a new mount with the same filesystem, flags, and peer group. Used to implement bind
  /// mounts.
  MountHandle clone_mount(const DirEntryHandle& new_root, MountFlags flags) const;

  /// Do a clone of the full mount hierarchy below this mount. Used for creating mount
  /// namespaces and creating copies to use for propagation.
  MountHandle clone_mount_recursive() const;

  MountFlags flags() const;

 public:
  // state_accessor!(Mount, state, Arc<Mount>);
  starnix_sync::RwLock<MountState>::RwLockReadGuard Read() const { return state_.Read(); }
  starnix_sync::RwLock<MountState>::RwLockWriteGuard Write() const { return state_.Write(); }

  // impl fmt::Debug
  mtl::BString debug() const;

  // C++
  fbl::RefPtr<DirEntry> GetKey() const;

  ~Mount();

 private:
  friend class ActiveNamespaceNode;
  friend class MountState;
  friend class Namespace;
  friend class NamespaceNode;
  friend class Submount;
  friend struct MountInfo;

  Mount(uint64_t id, MountFlags flags, DirEntryHandle root, FileSystemHandle fs);

 public:
  mtl::WeakPtrFactory<Mount> weak_factory_;  // must be last
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_H_
