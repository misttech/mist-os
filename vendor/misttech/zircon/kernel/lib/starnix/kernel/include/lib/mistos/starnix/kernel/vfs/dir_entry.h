// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_

#include <lib/fit/result.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/btree_map.h>
#include <lib/mistos/util/error_propagation.h>
#include <lib/starnix_sync/locks.h>

#include <utility>

#include <fbl/ref_counted_upgradeable.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

namespace unit_testing {
bool test_tmpfs();
}

namespace starnix {

class CurrentTask;
class DirEntry;
class FsNode;
class Mounts;

using DirEntryHandle = fbl::RefPtr<DirEntry>;
using FsNodeHandle = fbl::RefPtr<FsNode>;
using starnix_uapi::Errno;

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

  /// Whether the entry has filesystems mounted on top of it.
  bool has_mounts;
};

class DirEntryOps {
 public:
  /// Revalidate the [`DirEntry`], if needed.
  ///
  /// Most filesystems don't need to do any revalidations because they are "local"
  /// and all changes to nodes go through the kernel. However some filesystems
  /// allow changes to happen through other means (e.g. NFS, FUSE) and these
  /// filesystems need a way to let the kernel know it may need to refresh its
  /// cached metadata. This method provides that hook for such filesystems.
  ///
  /// For more details, see:
  ///  - https://www.halolinux.us/kernel-reference/the-dentry-cache.html
  ///  -
  ///  https://www.kernel.org/doc/html/latest/filesystems/path-lookup.html#revalidation-and-automounts
  ///  - https://lwn.net/Articles/649115/
  ///  - https://www.infradead.org/~mchehab/kernel_docs/filesystems/path-walking.html
  ///
  /// Returns `Ok(valid)` where `valid` indicates if the `DirEntry` is still valid,
  /// or an error.
  virtual fit::result<Errno, bool> revalidate(const CurrentTask& current_task, const DirEntry&);

  virtual ~DirEntryOps();
};

class DefaultDirEntryOps : public DirEntryOps {
 public:
  virtual ~DefaultDirEntryOps();
};

struct Created {};

template <typename CreateFn>
struct Existed {
 public:
  explicit Existed(CreateFn&& fn) : create_fn(std::forward<CreateFn>(fn)) {}

  CreateFn create_fn;
};

template <typename CreateFn>
struct CreationResult {
 public:
  explicit CreationResult(const Created& c) : variant_(c) {}
  explicit CreationResult(CreateFn&& fn)
      : variant_(Existed<CreateFn>(std::forward<CreateFn>(fn))) {}

 private:
  friend class DirEntry;

  ktl::variant<Created, Existed<CreateFn>> variant_;
};

// Helpers from the reference documentation for std::visit<>, to allow
// visit-by-overload of the std::variant<>
template <class... Ts>
struct overloaded : Ts... {
  using Ts::operator()...;
};

// explicit deduction guide (not needed as of C++20)
template <class... Ts>
overloaded(Ts...) -> overloaded<Ts...>;

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
class DirEntry : public fbl::RefCountedUpgradeable<DirEntry> {
 public:
  /// The FsNode referenced by this DirEntry.
  ///
  /// A given FsNode can be referenced by multiple DirEntry objects, for
  /// example if there are multiple hard links to a given FsNode.
  FsNodeHandle node_;

  /// The [`DirEntryOps`] for this `DirEntry`.
  ///
  /// The `DirEntryOps` are implemented by the individual file systems to provide
  /// specific behaviours for this `DirEntry`.
  ktl::unique_ptr<DirEntryOps> ops_;

  /// The mutable state for this DirEntry.
  ///
  /// Leaf lock - do not acquire other locks while holding this one.
  mutable starnix_sync::RwLock<DirEntryState> state_;

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
  using DirEntryChildren = util::BTreeMap<FsString, mtl::WeakPtr<DirEntry>>;
  mutable starnix_sync::RwLock<DirEntryChildren> children_;

  /// impl DirEntry
  static DirEntryHandle New(FsNodeHandle node, ktl::optional<DirEntryHandle> parent,
                            FsString local_name);

  /// Returns a new DirEntry for the given `node` without parent. The entry has no local name.
  static DirEntryHandle new_unrooted(FsNodeHandle node);

  class DirEntryLockedChildren {
   private:
    DirEntryHandle entry_;

    starnix_sync::RwLock<DirEntryChildren>::RwLockWriteGuard children_;

   public:
    DirEntryLockedChildren(
        DirEntryHandle entry,
        starnix_sync::RwLock<DirEntry::DirEntryChildren>::RwLockWriteGuard children)
        : entry_(ktl::move(entry)), children_(ktl::move(children)) {}

    /// impl<'a> DirEntryLockedChildren<'a>
    template <typename CreateNodeFn>
    fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> get_or_create_child(
        const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
        CreateNodeFn&& create_fn) {
      static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                          const FsNodeHandle&, const MountInfo&, const FsStr&>);

      auto create_child = [&](CreateNodeFn&& create_fn)
          -> fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> {
        // Before creating the child, check for existence.
        auto node_and_create_result =
            [&]() -> fit::result<Errno, ktl::pair<FsNodeHandle, CreationResult<CreateNodeFn>>> {
          auto lookup_result = entry_->node_->lookup(current_task, mount, name);
          if (lookup_result.is_ok()) {
            return fit::ok(
                ktl::pair(lookup_result.value(), CreationResult<CreateNodeFn>(create_fn)));
          }
          if (lookup_result.error_value().error_code() == ENOENT) {
            auto create_fn_result = create_fn(entry_->node_, mount, name) _EP(create_fn_result);
            return fit::ok(ktl::pair(create_fn_result.value(), Created()));
          }
          return lookup_result.take_error();
        }() _EP(node_and_create_result);
        auto [node, create_result] = node_and_create_result.value();

        ASSERT_MSG((node->info()->mode_ & FileMode::IFMT) != FileMode::EMPTY,
                   "FsNode initialization did not populate the FileMode in FsNodeInfo.");

        auto entry = DirEntry::New(node, {entry_}, name);

        // #[cfg(any(test, debug_assertions))]
        {
          // Take the lock on child while holding the one on the parent to ensure any wrong
          // ordering will trigger the tracing-mutex at the right call site.
          // auto _l1 = entry->state().Read();
        }

        return fit::ok(ktl::pair(entry, create_result));
      };

      auto result =
          [&]() -> fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> {
        auto it = children_->find(name);
        if (it == children_->end()) {
          // Vacant
          auto result = create_child(create_fn) _EP(result);
          auto [child, create_result] = result.value();
          children_->emplace(child->local_name(), child->weak_factory_.GetWeakPtr());
          return fit::ok(ktl::pair(child, create_result));
        }
        // Occupied
        // It's possible that the upgrade will succeed this time around because we dropped
        // the read lock before acquiring the write lock. Another thread might have
        // populated this entry while we were not holding any locks.
        auto child = it->second.Lock();
        if (child) {
          child->node_->fs()->did_access_dir_entry(child);
          return fit::ok(ktl::pair(child, CreationResult<CreateNodeFn>(create_fn)));
        }

        auto result = create_child(create_fn) _EP(result);
        auto [new_child, create_result] = result.value();
        children_->emplace(new_child->local_name(), new_child->weak_factory_.GetWeakPtr());
        return fit::ok(ktl::pair(new_child, create_result));
      }() _EP(result);

      auto [child, create_result] = result.value();
      child->node_->fs()->did_create_dir_entry(child);
      return fit::ok(ktl::pair(child, create_result));
    }
  };

 private:
  DirEntryLockedChildren lock_children();

 public:
  /// The name that this node's parent calls this node.
  ///
  /// If this node is mounted in a namespace, the parent of this node in that
  /// namespace might have a different name for the point in the namespace at
  /// which this node is mounted.
  FsString local_name() const;

  /// The parent DirEntry object.
  ///
  /// Returns None if this DirEntry is the root of its file system.
  ///
  /// Be aware that the root of one file system might be mounted as a child
  /// in another file system. For that reason, consider walking the
  /// NamespaceNode tree (which understands mounts) rather than the DirEntry
  /// tree.
  ktl::optional<DirEntryHandle> parent() const;

  /// The parent DirEntry object or this DirEntry if this entry is the root.
  ///
  /// Useful when traversing up the tree if you always want to find a parent
  /// (e.g., for "..").
  ///
  /// Be aware that the root of one file system might be mounted as a child
  /// in another file system. For that reason, consider walking the
  /// NamespaceNode tree (which understands mounts) rather than the DirEntry
  /// tree.
  DirEntryHandle parent_or_self();

  /// Whether this directory entry has been removed from the tree.
  bool is_dead() const { return state_.Read()->is_dead; }

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
                                                      const MountInfo& mount, const FsStr& name);

  /// Creates a new DirEntry
  ///
  /// The create_node_fn function is called to create the underlying FsNode
  /// for the DirEntry.
  ///
  /// If the entry already exists, create_node_fn is not called, and EEXIST is
  /// returned.
  template <typename CreateNodeFn>
  fit::result<Errno, DirEntryHandle> create_entry(const CurrentTask& current_task,
                                                  const MountInfo& mount, const FsStr& name,
                                                  CreateNodeFn&& fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    auto result = create_entry_internal(current_task, mount, name, fn) _EP(result);
    auto [entry, exists] = result.value();
    if (exists) {
      return fit::error(errno(EEXIST));
    }
    return fit::ok(entry);
  }

  /// Creates a new DirEntry. Works just like create_entry, except if the entry already exists,
  /// it is returned.
  template <typename CreateNodeFn>
  fit::result<Errno, DirEntryHandle> get_or_create_entry(const CurrentTask& current_task,
                                                         const MountInfo& mount, const FsStr& name,
                                                         CreateNodeFn&& fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    auto result = create_entry_internal(current_task, mount, name, fn) _EP(result);
    auto [entry, _exists] = result.value();
    return fit::ok(entry);
  }

  template <typename CreateNodeFn>
  fit::result<Errno, ktl::pair<DirEntryHandle, bool>> create_entry_internal(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      CreateNodeFn&& fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    if (DirEntry::is_reserved_name(name)) {
      return fit::error(errno(EEXIST));
    }

    // TODO: Do we need to check name for embedded NUL characters?
    if (name.size() > static_cast<size_t>(NAME_MAX)) {
      return fit::error(errno(ENAMETOOLONG));
    }
    if (starnix::contains(name, SEPARATOR)) {
      return fit::error(errno(EINVAL));
    }
    auto result = get_or_create_child(current_task, mount, name, fn) _EP(result);
    auto [entry, exists] = result.value();
    if (!exists) {
      // An entry was created. Update the ctime and mtime of this directory.

      // self.node.update_ctime_mtime();
      // entry.notify_creation();
    }
    return fit::ok(ktl::pair(entry, exists));
  }

  /// This is marked as test-only (private) because it sets the owner/group to root instead of the
  /// current user to save a bit of typing in tests, but this shouldn't happen silently in
  /// production.
  fit::result<Errno, DirEntryHandle> create_dir(const CurrentTask& current_task, const FsStr& name);

  // This function is for testing because it sets the owner/group to root instead of the current
  // user to save a bit of typing in tests, but this shouldn't happen silently in production.
  fit::result<Errno, DirEntryHandle> create_dir_for_testing(const CurrentTask& current_task,
                                                            const FsStr& name);

  /// Creates an anonymous file.
  ///
  /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
  ///
  /// Used by O_TMPFILE.
  fit::result<Errno, DirEntryHandle> create_tmpfile(const CurrentTask& current_task,
                                                    const MountInfo& mount, FileMode mode,
                                                    FsCred owner, OpenFlags flags);

  /*fit::result<Errno, void> unlink(const CurrentTask& current_task, const MountInfo& mount,
                                 const FsStr& name, UnlinkKind kind, bool must_be_directory);*/

  /// Destroy this directory entry.
  ///
  /// Notice that this method takes `self` by value to destroy this reference.
  /// Destroy this directory entry.
  ///
  /// Notice that this method takes `self` by value to destroy this reference.
  void destroy(const Mounts& mounts) &&;

  /// Returns whether this entry is a descendant of |other|.
  bool is_descendant_of(const DirEntryHandle& other) const;

  template <typename CreateNodeFn>
  fit::result<Errno, ktl::pair<DirEntryHandle, bool>> get_or_create_child(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      CreateNodeFn&& create_fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    ASSERT(!DirEntry::is_reserved_name(name));
    // Only directories can have children.
    if (!node_->is_dir()) {
      return fit::error(errno(ENOTDIR));
    }
    // The user must be able to search the directory (requires the EXEC permission)
    // self.node.check_access(current_task, mount, Access::EXEC)?;

    // Check if the child is already in children. In that case, we can
    // simply return the child and we do not need to call init_fn.
    auto child = [&]() -> ktl::optional<DirEntryHandle> {
      auto children_lock = children_.Read();
      auto it = children_lock->find(name);
      if (it != children_lock->end()) {
        auto strong_child = it->second.Lock();
        if (strong_child) {
          return strong_child;
        }
      }
      return ktl::nullopt;
    }();

    auto child_and_create_result =
        [&]() -> fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> {
      if (child.has_value()) {
        auto c = child.value();
        c->node_->fs()->did_access_dir_entry(c);
        return fit::ok(ktl::pair(c, CreationResult<CreateNodeFn>(create_fn)));
      }
      auto result =
          lock_children().get_or_create_child(current_task, mount, name, create_fn) _EP(result);
      auto [c, cr] = result.value();
      c->node_->fs()->purge_old_entries();
      return fit::ok(ktl::pair(c, ktl::move(cr)));
    }() _EP(child_and_create_result);

    auto [new_child, create_result] = child_and_create_result.value();
    auto new_result = [&]() -> fit::result<Errno, ktl::pair<DirEntryHandle, bool>> {
      return ktl::visit(
          overloaded{
              [&](const Created&) -> fit::result<Errno, ktl::pair<DirEntryHandle, bool>> {
                return fit::ok(ktl::pair(new_child, false));
              },
              [&](const Existed<CreateNodeFn>& e)
                  -> fit::result<Errno, ktl::pair<DirEntryHandle, bool>> {
                auto revalidate_result =
                    new_child->ops_->revalidate(current_task, *new_child) _EP(revalidate_result);

                if (revalidate_result.value()) {
                  return fit::ok(ktl::pair(new_child, true));
                }
                this->internal_remove_child(new_child.get());
                // child.destroy(&current_task.kernel().mounts);
                auto result = lock_children().get_or_create_child(current_task, mount, name,
                                                                  e.create_fn) _EP(result);
                auto [local_child, local_create_result] = result.value();
                local_child->node_->fs()->purge_old_entries();

                return fit::ok(ktl::pair(
                    local_child,
                    ktl::visit(overloaded{[](const Created&) -> bool { return false; },
                                          [](const Existed<CreateNodeFn>) -> bool { return true; }},
                               local_create_result.variant_)));
              },
          },
          create_result.variant_);
    }() _EP(new_result);

    auto [return_child, exists] = new_result.value();
    return fit::ok(ktl::pair(return_child, exists));
  }

  /// This function is only useful for tests and has some oddities.
  ///
  /// For example, not all the children might have been looked up yet, which
  /// means the returned vector could be missing some names.
  ///
  /// Also, the vector might have "extra" names that are in the process of
  /// being looked up. If the lookup fails, they'll be removed.
  fbl::Vector<FsString> copy_child_names();

 private:
  void internal_remove_child(DirEntry* child);

#if 0
  /// Notifies watchers on the current node and its parent about an event.
  void notify(InotifyMask event_mask) {
    notify_watchers(event_mask, is_dead());
  }

  /// Notifies watchers on the current node and its parent about an event.
  ///
  /// Used for FSNOTIFY_EVENT_INODE events, which ignore IN_EXCL_UNLINK.
  void notify_ignoring_excl_unlink(InotifyMask event_mask) {
    // We pretend that this directory entry is not dead to ignore IN_EXCL_UNLINK.
    notify_watchers(event_mask, false);
  }

  void notify_watchers(InotifyMask event_mask, bool is_dead) {
    auto mode = node_->info()->mode;
    if (auto parent = state_.Read()->parent) {
      parent->node_->watchers.notify(event_mask, 0, local_name(), mode, is_dead);
    }
    node_->watchers.notify(event_mask, 0, FsStr(), mode, is_dead);
  }

  /// Notifies parents about creation, and notifies current node about link_count change.
  void notify_creation() {
    auto mode = node_->info()->mode;
    if (fbl::RefPtr<DirEntry>::GetRefCount(this) > 1) {
      // Notify about link change only if there is already a hardlink.
      node_->watchers.notify(InotifyMask::ATTRIB, 0, FsStr(), mode, false);
    }
    if (auto parent = state_.Read()->parent) {
      parent->node_->watchers.notify(InotifyMask::CREATE, 0, local_name(), mode, false);
    }
  }

  /// Notifies watchers on the current node about deletion if this is the
  /// last hardlink, and drops the DirEntryHandle kept by Inotify.
  /// Parent is also notified about deletion.
  void notify_deletion() {
    auto mode = node_->info()->mode;
    if (!FileMode::is_dir(mode)) {
      // Linux notifies link count change for non-directories.
      node_->watchers.notify(InotifyMask::ATTRIB, 0, FsStr(), mode, false);
    }

    if (auto parent = state_.Read()->parent) {
      parent->node_->watchers.notify(InotifyMask::DELETE, 0, local_name(), mode, false);
    }

    // This check is incorrect if there's another hard link to this FsNode that isn't in
    // memory at the moment.
    if (fbl::RefPtr<DirEntry>::GetRefCount(this) == 1) {
      node_->watchers.notify(InotifyMask::DELETE_SELF, 0, FsStr(), mode, false);
    }
  }
#endif

 public:
  /// Returns true if this entry has mounts.
  bool has_mounts() const;

  /// Records whether or not the entry has mounts.
  void set_has_mounts(bool v);

 private:
  /// Verifies this directory has nothing mounted on it.
  fit::result<Errno> require_no_mounts(const MountInfo& parent_mount);

 public:
  // impl fmt::Debug for DirEntry
  mtl::BString debug() const;

  // C++
  /// The Drop trait for DirEntry removes the entry from the child list of the
  /// parent entry, which means we cannot drop DirEntry objects while holding a
  /// lock on the parent's child list.
  ~DirEntry();

  // WAVL-tree Index
  FsString GetKey() const;

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(DirEntry);

  friend bool unit_testing::test_tmpfs();

  DirEntry(FsNodeHandle node, ktl::unique_ptr<DirEntryOps> ops, DirEntryState state);

 public:
  mtl::WeakPtrFactory<DirEntry> weak_factory_;  // must be last
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_
