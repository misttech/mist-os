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

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<Errno, bool> DirEntryOps::revalidate(const CurrentTask& current_task, const DirEntry&) {
  return fit::ok(true);
}

DirEntryOps::~DirEntryOps() = default;

DefaultDirEntryOps::~DefaultDirEntryOps() = default;

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
  return DirEntry::DirEntryLockedChildren(fbl::RefPtr<DirEntry>(this),
                                          ktl::move(children_.Write()));
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

FsString DirEntry::GetKey() const { return local_name(); }

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

void DirEntry::destroy(const Mounts& mounts) && {
  /*bool unmount = [this]() {
    auto state = state_.Write();
    if (state->is_dead) {
      return false;
    }
    state->is_dead = true;
    return ktl::exchange(state->has_mounts, false);
  }();

  node_->fs()->will_destroy_dir_entry(this);
  if (unmount) {
    mounts.unmount(this);
  }
  //notify_deletion();
  */
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

fbl::Vector<FsString> DirEntry::copy_child_names() {
  fbl::Vector<FsString> child_names;
  auto c = children_.Read();
  for (auto iter = c->begin(); iter != c->end(); ++iter) {
    if (iter.IsValid()) {
      auto child = iter.CopyPointer().Lock();
      if (child) {
        fbl::AllocChecker ac;
        child_names.push_back(child->local_name(), &ac);
        ZX_ASSERT(ac.check());
      }
    }
  }
  return child_names;
}

void DirEntry::internal_remove_child(DirEntry* child) {
  auto local_name = child->local_name();

  LTRACEF_LEVEL(2, "local_name=[%.*s]\n", static_cast<int>(local_name.length()), local_name.data());

  auto children = this->children_.Write();
  if (auto weak_child = children->find(local_name);
      weak_child.IsValid() && weak_child != children->end()) {
    // If this entry is occupied, we need to check whether child is
    // the current occupant. If so, we should remove the entry
    // because the child no longer exists.
    if (weak_child.CopyPointer().get() == child) {
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
