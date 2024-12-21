// Copyright 2024 Mist Tecnologia. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/dir_entry.h"

#include <lib/fit/result.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <trace.h>
#include <zircon/assert.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/unique_ptr.h>
#include <lockdep/guard.h>

#include "../kernel_priv.h"

namespace ktl {

using std::addressof;
using std::destroy_at;
using std::hash;

}  // namespace ktl

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<Errno, bool> DirEntryOps::revalidate(const CurrentTask& current_task, const DirEntry&) {
  return fit::ok(true);
}

DirEntryOps::~DirEntryOps() = default;

DefaultDirEntryOps::~DefaultDirEntryOps() = default;

// Define the key extraction function
size_t DirEntry::GetKey() const { return reinterpret_cast<size_t>(this); }

// Define the hash function for the key
size_t DirEntry::GetHash(const size_t key) { return ktl::hash<size_t>()(key); }

DirEntry::~DirEntry() {
  LTRACE_ENTRY_OBJ;

  auto local_name = state_.Read()->local_name;
  LTRACEF_LEVEL(2, "local_name=[%.*s]\n", static_cast<int>(local_name.size()), local_name.data());

  auto maybe_parent = ktl::move(state_.Write()->parent);
  if (maybe_parent.has_value()) {
    const auto& parent = maybe_parent.value();
    parent->internal_remove_child(this);
  }

  LTRACE_EXIT_OBJ;
}

DirEntryHandle DirEntry::New(FsNodeHandle node, ktl::optional<DirEntryHandle> parent,
                             FsString local_name) {
  LTRACEF_LEVEL(2, "local_name=[%.*s]\n", static_cast<int>(local_name.size()), local_name.data());
  fbl::AllocChecker ac;
  auto ops = ktl::make_unique<DefaultDirEntryOps>(&ac);
  ZX_ASSERT(ac.check());
  auto result = fbl::AdoptRef(new (&ac) DirEntry(ktl::move(node), ktl::move(ops),
                                                 {.parent = ktl::move(parent),
                                                  .local_name = local_name,
                                                  .is_dead = false,
                                                  .has_mounts = false}));
  ZX_ASSERT(ac.check());

  // #[cfg(any(test, debug_assertions))]
  {
    result->children_.Read();
    result->state_.Read();
  }
  return result;
}

DirEntryHandle DirEntry::new_unrooted(FsNodeHandle node) { return New(ktl::move(node), {}, {}); }

DirEntry::DirEntryLockedChildren DirEntry::lock_children() {
  fbl::RefPtr<DirEntry> self(this);
  return DirEntry::DirEntryLockedChildren(self, ktl::move(children_.Write()));
}

FsString DirEntry::local_name() const { return state_.Read()->local_name; }

ktl::optional<DirEntryHandle> DirEntry::parent() const { return state_.Read()->parent; }

DirEntryHandle DirEntry::parent_or_self() {
  auto parent = state_.Read()->parent;
  return parent ? parent.value() : fbl::RefPtr<DirEntry>(this);
}

fit::result<Errno, DirEntryHandle> DirEntry::component_lookup(const CurrentTask& current_task,
                                                              const MountInfo& mount,
                                                              const FsStr& name) {
  LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.length()), name.data());

  auto create_fn = [&current_task](const FsNodeHandle& d, const MountInfo& mount,
                                   const FsStr& name) -> fit::result<Errno, FsNodeHandle> {
    LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.length()), name.data());
    return d->lookup(current_task, mount, name);
  };

  auto result = get_or_create_child(current_task, mount, name, create_fn) _EP(result);
  auto [node, _] = result.value();
  return fit::ok(node);
}

DirEntry::DirEntry(FsNodeHandle node, ktl::unique_ptr<DirEntryOps> ops, DirEntryState state)
    : node_(ktl::move(node)), ops_(ktl::move(ops)), state_(ktl::move(state)), weak_factory_(this) {
  LTRACE_ENTRY_OBJ;
}

fit::result<Errno, DirEntryHandle> DirEntry::create_dir(const CurrentTask& current_task,
                                                        const FsStr& name) {
  return create_dir_for_testing(current_task, name);
}

fit::result<Errno, DirEntryHandle> DirEntry::create_dir_for_testing(const CurrentTask& current_task,
                                                                    const FsStr& name) {
  // TODO: apply_umask
  return create_entry(current_task, MountInfo::detached(), name,
                      [&current_task](const FsNodeHandle& dir, const MountInfo& mount,
                                      const FsStr& name) -> fit::result<Errno, FsNodeHandle> {
                        return dir->mknod(current_task, mount, name, FILE_MODE(IFDIR, 0777),
                                          DeviceType::NONE, FsCred::root());
                      });
}

fit::result<Errno> DirEntry::unlink(const CurrentTask& current_task, const MountInfo& mount,
                                    const FsStr& name, UnlinkKind kind, bool must_be_directory) {
  LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.length()), name.data());

  ZX_ASSERT(!DirEntry::is_reserved_name(name));

  // child *must* be dropped after self_children and child_children below (even in the error paths)
  DirEntryHandle child;

  auto self_children = lock_children();
  auto child_result = self_children.component_lookup(current_task, mount, name) _EP(child_result);
  child = child_result.value();
  auto child_children = child->children_.Read();

  _EP(child->require_no_mounts(mount));

  // Check that this filesystem entry must be a directory. This can
  // happen if the path terminates with a trailing slash.
  //
  // Example: If we're unlinking a symlink `/foo/bar/`, this would
  // result in `ENOTDIR` because of the trailing slash, even if
  // `UnlinkKind::NonDirectory` was used.
  if (must_be_directory && !child->node_->is_dir()) {
    return fit::error(errno(ENOTDIR));
  }

  switch (kind) {
    case UnlinkKind::Directory:
      if (!child->node_->is_dir()) {
        return fit::error(errno(ENOTDIR));
      }
      // This check only covers whether the cache is non-empty.
      // We actually need to check whether the underlying directory is
      // empty by asking the node via remove below.
      if (!child_children->empty()) {
        return fit::error(errno(ENOTEMPTY));
      }
      break;

    case UnlinkKind::NonDirectory:
      if (child->node_->is_dir()) {
        return fit::error(errno(EISDIR));
      }
      break;
  }

  _EP(node_->unlink(current_task, mount, name, child->node_));
  self_children.children_->erase(name);

  ktl::destroy_at(ktl::addressof(child_children));
  ktl::destroy_at(ktl::addressof(self_children));

  child->destroy(current_task->kernel()->mounts_);

  return fit::ok();
}

void DirEntry::destroy(const Mounts& mounts) {
  bool unmount = [this]() {
    auto state = state_.Write();
    if (state->is_dead) {
      return false;
    }
    state->is_dead = true;
    return ktl::exchange(state->has_mounts, false);
  }();

  node_->fs()->will_destroy_dir_entry(fbl::RefPtr<DirEntry>(this));
  if (unmount) {
    mounts.unmount(*this);
  }
  // notify_deletion();
}

bool DirEntry::is_descendant_of(const DirEntryHandle& other) const {
  auto current = fbl::RefPtr<const DirEntry>(this);
  while (true) {
    if (current.get() == other.get()) {
      // We found |other|.
      return true;
    }
    auto next = current->parent();
    if (next.has_value()) {
      current = next.value();
    } else {
      // We reached the root of the file system.
      return false;
    }
  }
}

// Helper class to manage locking parent directories during rename operations
class RenameGuard {
 public:
  static RenameGuard lock(const DirEntryHandle& old_parent, const DirEntryHandle& new_parent) {
    if (old_parent.get() == new_parent.get()) {
      return RenameGuard(old_parent->lock_children(), {});
    }
    // Following gVisor, these locks are taken in ancestor-to-descendant order.
    // Moreover, if the nodes are not comparable, they are taken from smallest inode to
    // biggest.
    if (new_parent->is_descendant_of(old_parent) ||
        (!old_parent->is_descendant_of(new_parent) &&
         old_parent->node_->node_id_ < new_parent->node_->node_id_)) {
      auto old_parent_guard = old_parent->lock_children();
      auto new_parent_guard = new_parent->lock_children();
      return RenameGuard(
          ktl::move(old_parent_guard),
          ktl::optional<DirEntry::DirEntryLockedChildren>{ktl::move(new_parent_guard)});
    }

    auto new_parent_guard = new_parent->lock_children();
    auto old_parent_guard = old_parent->lock_children();
    return RenameGuard(ktl::move(old_parent_guard), ktl::optional<DirEntry::DirEntryLockedChildren>{
                                                        ktl::move(new_parent_guard)});
  }

  DirEntry::DirEntryLockedChildren& old_parent() { return old_parent_guard_; }

  DirEntry::DirEntryLockedChildren& new_parent() {
    if (new_parent_guard_.has_value()) {
      return new_parent_guard_.value();
    }
    return old_parent_guard_;
  }

 private:
  RenameGuard(DirEntry::DirEntryLockedChildren old_parent_guard,
              ktl::optional<DirEntry::DirEntryLockedChildren> new_parent_guard)
      : old_parent_guard_(ktl::move(old_parent_guard)),
        new_parent_guard_(ktl::move(new_parent_guard)) {}

  DirEntry::DirEntryLockedChildren old_parent_guard_;
  ktl::optional<DirEntry::DirEntryLockedChildren> new_parent_guard_;
};

fit::result<Errno> DirEntry::rename(const CurrentTask& current_task,
                                    const DirEntryHandle& old_parent, const MountInfo& old_mount,
                                    const FsStr& old_basename, const DirEntryHandle& new_parent,
                                    const MountInfo& new_mount, const FsStr& new_basename,
                                    RenameFlags flags) {
  // The nodes we are touching must be part of the same mount.
  if (old_mount != new_mount) {
    return fit::error(errno(EXDEV));
  }
  // The mounts are equals, choose one.
  const MountInfo& mount = old_mount;

  // If either the old_basename or the new_basename is a reserved name
  // (e.g., "." or ".."), then we cannot do the rename.
  if (DirEntry::is_reserved_name(old_basename) || DirEntry::is_reserved_name(new_basename)) {
    if (flags.contains(RenameFlagsEnum::NOREPLACE)) {
      return fit::error(errno(EEXIST));
    }
    return fit::error(errno(EBUSY));
  }

  // If the names and parents are the same, then there's nothing to do
  // and we can report success.
  if ((old_parent.get() == new_parent.get()) && (old_basename == new_basename)) {
    return fit::ok();
  }

  // This task must have write access to the old and new parent nodes.
  _EP(old_parent->node_->check_access(current_task, mount, Access(AccessEnum::WRITE),
                                      CheckAccessReason::InternalPermissionChecks));
  _EP(new_parent->node_->check_access(current_task, mount, Access(AccessEnum::WRITE),
                                      CheckAccessReason::InternalPermissionChecks));

  // The mount check ensures that the nodes we're touching are part of the
  // same file system. It doesn't matter where we grab the FileSystem reference from.
  auto fs = old_parent->node_->fs();

  // We need to hold these DirEntryHandles until after we drop all the
  // locks so that we do not deadlock when we drop them.
  DirEntryHandle renamed;
  ktl::optional<DirEntryHandle> maybe_replaced;

  {
    // Before we take any locks, we need to take the rename mutex on
    // the file system. This lock ensures that no other rename
    // operations are happening in this file system while we're
    // analyzing this rename operation.
    auto _lock = fs->rename_mutex_.Lock();

    // Compute the list of ancestors of new_parent to check whether new_parent is a
    // descendant of the renamed node. This must be computed before taking any lock to
    // prevent lock inversions.
    fbl::Vector<DirEntryHandle> new_parent_ancestor_list;
    {
      auto current = new_parent;
      while (current) {
        auto parent = current->parent();
        if (parent.has_value()) {
          fbl::AllocChecker ac;
          new_parent_ancestor_list.push_back(parent.value(), &ac);
          if (!ac.check()) {
            return fit::error(errno(ENOMEM));
          }
          current = parent.value();
        } else {
          break;
        }
      }
    }

    // We cannot simply grab the locks on old_parent and new_parent
    // independently because old_parent and new_parent might be the
    // same directory entry. Instead, we use the RenameGuard helper to
    // grab the appropriate locks.
    auto state = RenameGuard::lock(old_parent, new_parent);

    // Now that we know the old_parent child list cannot change, we
    // establish the DirEntry that we are going to try to rename.
    auto lookup_result =
        state.old_parent().component_lookup(current_task, mount, old_basename) _EP(lookup_result);
    renamed = lookup_result.value();

    // Check whether the sticky bit on the old parent prevents us from
    // removing this child.
    _EP(old_parent->node_->check_sticky_bit(current_task, renamed->node_));

    // If new_parent is a descendant of renamed, the operation would
    // create a cycle. That's disallowed.
    for (const auto& ancestor : new_parent_ancestor_list) {
      if (ancestor.get() == renamed.get()) {
        return fit::error(errno(EINVAL));
      }
    }

    // Check whether the renamed entry is a mountpoint.
    // TODO: We should hold a read lock on the mount points for this
    //       namespace to prevent the child from becoming a mount point
    //       while this function is executing.
    _EP(renamed->require_no_mounts(mount));

    // We need to check if there is already a DirEntry with
    // new_basename in new_parent. If so, there are additional checks
    // we need to perform.
    lookup_result = state.new_parent().component_lookup(current_task, mount, new_basename);
    if (lookup_result.is_ok()) {
      // Set `maybe_replaced` now to ensure it gets dropped in the right order.
      auto replaced = lookup_result.value();
      maybe_replaced = replaced;

      if (flags.contains(RenameFlagsEnum::NOREPLACE)) {
        return fit::error(errno(EEXIST));
      }

      // Sayeth https://man7.org/linux/man-pages/man2/rename.2.html:
      //
      // "If oldpath and newpath are existing hard links referring to the
      // same file, then rename() does nothing, and returns a success
      // status."
      if (renamed->node_.get() == replaced->node_.get()) {
        return fit::ok();
      }

      // Sayeth https://man7.org/linux/man-pages/man2/rename.2.html:
      //
      // oldpath can specify a directory. In this case, newpath must
      // either not exist, or it must specify an empty directory.
      if (replaced->node_->is_dir()) {
        // Check whether the replaced entry is a mountpoint.
        // TODO: We should hold a read lock on the mount points for this
        //       namespace to prevent the child from becoming a mount point
        //       while this function is executing.
        _EP(replaced->require_no_mounts(mount));
      }

      if (!(flags.intersects(RenameFlags(RenameFlagsEnum::EXCHANGE) |
                             RenameFlagsEnum::REPLACE_ANY))) {
        if (renamed->node_->is_dir() && !replaced->node_->is_dir()) {
          return fit::error(errno(ENOTDIR));
        } else if (!renamed->node_->is_dir() && replaced->node_->is_dir()) {
          return fit::error(errno(EISDIR));
        }
      }
    } else if (lookup_result.error_value().error_code() == ENOENT) {
      // It's fine for the lookup to fail to find a child.
    } else {
      // However, other errors are fatal.
      return lookup_result.take_error();
    }

    // We've found all the errors that we know how to find. Ask the
    // file system to actually execute the rename operation. Once the
    // file system has executed the rename, we are no longer allowed to
    // fail because we will not be able to return the system to a
    // consistent state.

    if (flags.contains(RenameFlagsEnum::EXCHANGE)) {
      if (!maybe_replaced.has_value()) {
        return fit::error(errno(ENOENT));
      }
      auto replaced = maybe_replaced.value();
      _EP(fs->exchange(current_task, renamed->node_, old_parent->node_, old_basename,
                       replaced->node_, new_parent->node_, new_basename));
    } else {
      _EP(fs->rename(current_task, old_parent->node_, old_basename, new_parent->node_, new_basename,
                     renamed->node_,
                     maybe_replaced.has_value() ? maybe_replaced.value()->node_ : nullptr));
    }

    {
      // Update the parent and local name for the DirEntry we are renaming
      // to reflect its new parent and its new name.
      auto renamed_state = renamed->state_.Write();
      renamed_state->parent = new_parent;
      renamed_state->local_name = new_basename;
    }
    // Actually add the renamed child to the new_parent's child list.
    // This operation implicitly removes the replaced child (if any)
    // from the child list.
    state.new_parent().children_->emplace(renamed->local_name(),
                                          renamed->weak_factory_.GetWeakPtr());

    if (flags.contains(RenameFlagsEnum::EXCHANGE)) {
      // Reparent `replaced` when exchanging.
      ZX_ASSERT_MSG(maybe_replaced.has_value(), "replaced expected with RENAME_EXCHANGE");
      auto replaced = maybe_replaced.value();
      {
        auto replaced_state = replaced->state_.Write();
        replaced_state->parent = old_parent;
        replaced_state->local_name = old_basename;
      }
      state.old_parent().children_->emplace(replaced->local_name(),
                                            replaced->weak_factory_.GetWeakPtr());
    } else {
      // Remove the renamed child from the old_parent's child list.
      state.old_parent().children_->erase(old_basename);
    }
  }

  fs->purge_old_entries();

  if (maybe_replaced.has_value()) {
    if (!flags.contains(RenameFlagsEnum::EXCHANGE)) {
      (*maybe_replaced)->destroy(current_task->kernel()->mounts_);
    }
  }

  // Renaming a file updates its ctime
  // renamed->node_->update_ctime();

  // auto mode = renamed->node_->info()->mode_;
  // auto cookie = current_task->kernel()->get_next_inotify_cookie();
  // old_parent->node_->watchers()->notify(InotifyMask::MOVE_FROM, cookie, old_basename, mode,
  // false); new_parent->node_->watchers()->notify(InotifyMask::MOVE_TO, cookie, new_basename,
  // mode, false); renamed->node_->watchers()->notify(InotifyMask::MOVE_SELF, 0, FsString(), mode,
  // false);

  return fit::ok();
}

fbl::Vector<FsString> DirEntry::copy_child_names() const {
  fbl::Vector<FsString> child_names;
  auto c = children_.Read();
  for (auto iter = c->begin(); iter != c->end(); ++iter) {
    auto child = iter->second.Lock();
    if (child) {
      fbl::AllocChecker ac;
      child_names.push_back(child->local_name(), &ac);
      ZX_ASSERT(ac.check());
    }
  }
  return child_names;
}

void DirEntry::internal_remove_child(DirEntry* child) const {
  auto local_name = child->local_name();

  LTRACEF_LEVEL(2, "local_name=[%.*s]\n", static_cast<int>(local_name.length()), local_name.data());

  auto children = this->children_.Write();
  if (auto weak_child = children->find(local_name); weak_child != children->end()) {
    // If this entry is occupied, we need to check whether child is
    // the current occupant. If so, we should remove the entry
    // because the child no longer exists.
    if (weak_child->second.get() == child) {
      children->erase(local_name);
    }
  }
}

bool DirEntry::has_mounts() const { return state_.Read()->has_mounts; }

/// Records whether or not the entry has mounts.
void DirEntry::set_has_mounts(bool v) { state_.Write()->has_mounts = v; }

/// Verifies this directory has nothing mounted on it.
fit::result<Errno> DirEntry::require_no_mounts(const MountInfo& parent_mount) {
  if (state_.Read()->has_mounts) {
    ktl::optional<MountHandle> mount_handle = *parent_mount;
    if (mount_handle.has_value()) {
      if ((*mount_handle)->has_submount(fbl::RefPtr<DirEntry>(this))) {
        return fit::error(errno(EBUSY));
      }
    }
  }
  return fit::ok();
}

BString DirEntry::debug() const {
  fbl::Vector<BString> parents;
  auto maybe_parent = state_.Read()->parent;
  while (maybe_parent) {
    fbl::AllocChecker ac;
    parents.push_back(maybe_parent.value()->local_name(), &ac);
    ZX_ASSERT(ac.check());
    maybe_parent = maybe_parent.value()->parent();
  }

  auto name = local_name();
  auto vec_fmt = mtl::to_string(parents);

  return mtl::format("DirEntry {id=%p, local_name=%.*s, parent=%.*s}", this,
                     static_cast<int>(name.size()), name.data(), static_cast<int>(vec_fmt.size()),
                     vec_fmt.data());
}

}  // namespace starnix
