// Copyright 2024 Mist Tecnologia. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/dir_entry.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/util/weak_wrapper.h>
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

/// The Drop trait for DirEntry removes the entry from the child list of the
/// parent entry, which means we cannot drop DirEntry objects while holding a
/// lock on the parent's child list.
DirEntry::~DirEntry() {
  auto local_name = state_.Read()->local_name;
  LTRACEF_LEVEL(2, "local_name=[%.*s]\n", static_cast<int>(local_name.size()), local_name.data());

  auto maybe_parent = state_.Write()->parent;
  if (maybe_parent.has_value()) {
    auto parent = maybe_parent.value();
    parent->internal_remove_child(this);
  }
  children_.Write()->clear();
}

DirEntryHandle DirEntry::New(FsNodeHandle node, ktl::optional<DirEntryHandle> parent,
                             FsString local_name) {
  LTRACEF_LEVEL(2, "local_name=[%.*s]\n", static_cast<int>(local_name.size()), local_name.data());
  fbl::AllocChecker ac;
  auto ops = ktl::make_unique<DefaultDirEntryOps>(&ac);
  ZX_ASSERT(ac.check());
  auto result = fbl::AdoptRef(new (&ac) DirEntry(
      node, ktl::move(ops),
      {.parent = parent, .local_name = local_name, .is_dead = false, .mount_count = 0}));
  ZX_ASSERT(ac.check());

  // #[cfg(any(test, debug_assertions))]
  {
    result->children_.Read();
    result->state_.Read();
  }
  return result;
}

DirEntryHandle DirEntry::new_unrooted(FsNodeHandle node) { return New(node, {}, {}); }

DirEntry::DirEntryLockedChildren DirEntry::lock_children() {
  return DirEntry::DirEntryLockedChildren(fbl::RefPtr<DirEntry>(this),
                                          ktl::move(children_.Write()));
}

FsString DirEntry::local_name() const { return state_.Read()->local_name; }

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

  auto get_or_create_child_result =
      get_or_create_child(current_task, mount, name, create_fn) _EP(get_or_create_child_result);
  auto [node, _] = get_or_create_child_result.value();
  return fit::ok(node);
}

FsString DirEntry::GetKey() const { return local_name(); }

DirEntry::DirEntry(FsNodeHandle node, ktl::unique_ptr<DirEntryOps> ops, DirEntryState state)
    : node_(ktl::move(node)), ops_(ktl::move(ops)), state_(ktl::move(state)) {}

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

}  // namespace starnix
