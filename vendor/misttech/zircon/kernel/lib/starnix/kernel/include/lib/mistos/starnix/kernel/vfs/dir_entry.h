// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/weak_wrapper.h>

#include <functional>
#include <utility>
#include <vector>

#include <fbl/auto_lock.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <lockdep/guard.h>
#include <zxtest/cpp/zxtest_prod.h>

namespace starnix {

struct DirEntryState {
  /// The parent DirEntry.
  ///
  /// The DirEntry tree has strong references from child-to-parent and weak
  /// references from parent-to-child. This design ensures that the parent
  /// chain is always populated in the cache, but some children might be
  /// missing from the cache.
  ktl::optional<DirEntryHandle> parent;

  /// The name that this parent calls this child.
  ///
  /// This name might not be reflected in the full path in the namespace that
  /// contains this DirEntry. For example, this DirEntry might be the root of
  /// a chroot.
  ///
  /// Most callers that want to work with names for DirEntries should use the
  /// NamespaceNodes.
  FsString local_name;

  /// Whether this directory entry has been removed from the tree.
  bool is_dead;

  /// The number of filesystem mounted on the directory entry.
  uint32_t mount_count;
};

class DirEntryLockedChildren;

/// An entry in a directory.
///
/// This structure assigns a name to an FsNode in a given file system. An
/// FsNode might have multiple directory entries, for example if there are more
/// than one hard link to the same FsNode. In those cases, each hard link will
/// have a different parent and a different local_name because each hard link
/// has its own DirEntry object.
///
/// A directory cannot have more than one hard link, which means there is a
/// single DirEntry for each Directory FsNode. That invariant lets us store the
/// children for a directory in the DirEntry rather than in the FsNode.
class DirEntry
    : public fbl::WAVLTreeContainable<util::WeakPtr<DirEntry>, fbl::NodeOptions::AllowClearUnsafe>,
      private fbl::RefCountedUpgradeable<DirEntry> {
 public:
  using DirEntryChildren = fbl::WAVLTree<FsString, util::WeakPtr<DirEntry>>;

  /// The FsNode referenced by this DirEntry.
  ///
  /// A given FsNode can be referenced by multiple DirEntry objects, for
  /// example if there are multiple hard links to a given FsNode.
  FsNodeHandle node;

  /// The mutable state for this DirEntry.
  ///
  /// Leaf lock - do not acquire other locks while holding this one.
  mutable DECLARE_MUTEX(DirEntry) dir_entry_state_rw_lock;
  DirEntryState state TA_GUARDED(dir_entry_state_rw_lock);

  /// A partial cache of the children of this DirEntry.
  ///
  /// DirEntries are added to this cache when they are looked up and removed
  /// when they are no longer referenced.
  ///
  /// This is separated from the DirEntryState for lock ordering. rename needs to lock the source
  /// parent, the target parent, the source, and the target - four (4) DirEntries in total.
  /// Getting the ordering right on these is nearly impossible. However, we only need to lock the
  /// children map on the two parents and we don't need to lock the children map on the two
  /// children. So splitting the children out into its own lock resolves this.
  mutable DECLARE_MUTEX(DirEntry) dir_entry_childern_rw_lock;
  mutable DirEntryChildren children TA_GUARDED(dir_entry_childern_rw_lock);

  /// impl DirEntry
  static DirEntryHandle New(FsNodeHandle node, ktl::optional<DirEntryHandle> parent,
                            FsString local_name);

  /// Returns a new DirEntry for the given `node` without parent. The entry has no local name.
  static DirEntryHandle new_unrooted(FsNodeHandle node);

 private:
  DirEntryLockedChildren lock_children() const;

 public:
  /// The name that this node's parent calls this node.
  ///
  /// If this node is mounted in a namespace, the parent of this node in that
  /// namespace might have a different name for the point in the namespace at
  /// which this node is mounted.
  FsString local_name() const {
    Guard<Mutex> lock(&dir_entry_state_rw_lock);
    return state.local_name;
  }

  /// Whether the given name has special semantics as a directory entry.
  ///
  /// Specifically, whether the name is empty (which means "self"), dot
  /// (which also means "self"), or dot dot (which means "parent").
  static bool is_reserved_name(const FsStr& name) {
    return name.empty() || name == "." || name == "..";
  }

  /// Look up a directory entry with the given name as direct child of this
  /// entry.
  fit::result<Errno, DirEntryHandle> component_lookup(const CurrentTask& current_task,
                                                      const MountInfo& mount,
                                                      const FsStr& name) const;

  /// Creates a new DirEntry
  ///
  /// The create_node_fn function is called to create the underlying FsNode
  /// for the DirEntry.
  ///
  /// If the entry already exists, create_node_fn is not called, and EEXIST is
  /// returned.
  fit::result<Errno, DirEntryHandle> create_entry(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                     const FsStr&)>
          create_node_fn);

  /// Creates a new DirEntry. Works just like create_entry, except if the entry already exists,
  /// it is returned.
  fit::result<Errno, DirEntryHandle> get_or_create_entry(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                     const FsStr&)>
          create_node_fn);

  fit::result<Errno, std::pair<DirEntryHandle, bool>> create_entry_internal(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                     const FsStr&)>
          create_node_fn);

 private:
  /// This is marked as test-only (private) because it sets the owner/group to root instead of the
  /// current user to save a bit of typing in tests, but this shouldn't happen silently in
  /// production.
  fit::result<Errno, DirEntryHandle> create_dir(const CurrentTask& current_task, const FsStr& name);

 public:
  fit::result<Errno, std::pair<DirEntryHandle, bool>> get_or_create_child(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                     const FsStr&)>
          create_node_fn) const;

  /// This function is only useful for tests and has some oddities.
  ///
  /// For example, not all the children might have been looked up yet, which
  /// means the returned vector could be missing some names.
  ///
  /// Also, the vector might have "extra" names that are in the process of
  /// being looked up. If the lookup fails, they'll be removed.
  std::vector<FsString> copy_child_names();

  // C++
 public:
  ~DirEntry();
  using fbl::RefCountedUpgradeable<DirEntry>::AddRef;
  using fbl::RefCountedUpgradeable<DirEntry>::Release;
  using fbl::RefCountedUpgradeable<DirEntry>::Adopt;
  using fbl::RefCountedUpgradeable<DirEntry>::AddRefMaybeInDestructor;

  // WAVL-tree Index
  FsString GetKey() const { return local_name(); }

 private:
  ZXTEST_FRIEND_TEST(TmpFs, test_tmpfs);

  DirEntry(FsNodeHandle node, DirEntryState state);
};

class DirEntryLockedChildren {
 private:
  DirEntryHandle entry_;
  fbl::AutoLock<Mutex> lock_;
  DirEntry::DirEntryChildren* children_;

 public:
  DirEntryLockedChildren(DirEntryHandle entry, Mutex* lock, DirEntry::DirEntryChildren* children)
      : entry_(std::move(entry)), lock_(lock), children_(children) {}

  /// impl<'a> DirEntryLockedChildren<'a>
  fit::result<Errno, std::pair<DirEntryHandle, bool>> get_or_create_child(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                     const FsStr&)>
          create_fn);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_
