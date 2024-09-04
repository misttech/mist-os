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
#include <lockdep/guard.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

DirEntry::~DirEntry() { children.Write()->clear_unsafe(); }

DirEntryHandle DirEntry::New(FsNodeHandle node, ktl::optional<DirEntryHandle> parent,
                             FsString local_name) {
  fbl::AllocChecker ac;
  auto handle = fbl::AdoptRef(new (&ac) DirEntry(node, {parent, local_name, false, 0}));
  ZX_ASSERT(ac.check());
  return handle;
}

DirEntryHandle DirEntry::new_unrooted(FsNodeHandle node) { return New(node, {}, {}); }

FsString DirEntry::local_name() const { return state.Read()->local_name; }

fit::result<Errno, DirEntryHandle> DirEntry::component_lookup(const CurrentTask& current_task,
                                                              const MountInfo& mount,
                                                              const FsStr& name) const {
  LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.length()), name.data());

  auto get_or_create_child_result = get_or_create_child(
      current_task, mount, name,
      [&](const FsNodeHandle& d, const MountInfo& mount, const FsStr& name) -> auto {
        // LTRACEF_LEVEL(2, "name=%s\n", name.c_str());
        return d->lookup(current_task, mount, name);
      });

  if (get_or_create_child_result.is_error())
    return get_or_create_child_result.take_error();

  auto [_node, _] = get_or_create_child_result.value();
  return fit::ok(_node);
}

FsString DirEntry::GetKey() const { return local_name(); }

DirEntry::DirEntry(FsNodeHandle node, DirEntryState state)
    : node(ktl::move(node)), state(ktl::move(state)) {}

#if 0
fit::result<Errno, DirEntryHandle> DirEntry::create_entry(
    const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
    std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                   const FsStr&)>
        create_node_fn) {
  auto result = create_entry_internal(current_task, mount, name, create_node_fn);
  if (result.is_error()) {
    return result.take_error();
  }

  auto [entry, exists] = result.value();
  if (exists) {
    return fit::error(errno(EEXIST));
  }
  return fit::ok(entry);
}

fit::result<Errno, DirEntryHandle> DirEntry::get_or_create_entry(
    const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
    std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                   const FsStr&)>
        create_node_fn) {
  auto result = create_entry_internal(current_task, mount, name, create_node_fn);
  if (result.is_error()) {
    return result.take_error();
  }
  auto [entry, _] = result.value();
  return fit::ok(entry);
}

fit::result<Errno, std::pair<DirEntryHandle, bool>> DirEntry::create_entry_internal(
    const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
    std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                   const FsStr&)>
        create_node_fn) {
  if (DirEntry::is_reserved_name(name)) {
    return fit::error(errno(EEXIST));
  }

  // TODO: Do we need to check name for embedded NUL characters?
  if (name.size() > static_cast<size_t>(NAME_MAX)) {
    return fit::error(errno(ENAMETOOLONG));
  }

  /*if (name
    .contains(SEPARATOR)) { return fit::error(errno(EINVAL)); }
    */

  auto result = get_or_create_child(current_task, mount, name, create_node_fn);
  if (result.is_error()) {
    return result.take_error();
  }

  auto [entry, exists] = result.value();
  if (!exists) {
    // An entry was created. Update the ctime and mtime of this directory.
    // self.node.update_ctime_mtime();
    // entry.notify_creation();
  }
  return fit::ok(std::make_pair(entry, exists));
}
#endif

fit::result<Errno, DirEntryHandle> DirEntry::create_dir(const CurrentTask& current_task,
                                                        const FsStr& name) {
  return create_entry(current_task, MountInfo::detached(), name,
                      [&current_task](const FsNodeHandle& dir, const MountInfo& mount,
                                      const FsStr& name) -> fit::result<Errno, FsNodeHandle> {
                        return dir->mknod(current_task, mount, name, FILE_MODE(IFDIR, 0777),
                                          DeviceType::NONE, FsCred::root());
                      });
}

#if 0
fit::result<Errno, std::pair<DirEntryHandle, bool>> DirEntry::get_or_create_child(
    const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
    std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                   const FsStr&)>
        create_node_fn) const {
  // LTRACEF_LEVEL(2, "name=%s\n", name.c_str());
  ASSERT(!DirEntry::is_reserved_name(name));
  // Only directories can have children.
  if (!node->is_dir()) {
    return fit::error(errno(ENOTDIR));
  }
  // The user must be able to search the directory (requires the EXEC permission)
  // self.node.check_access(current_task, mount, Access::EXEC)?;

  // Check if the child is already in children. In that case, we can
  // simply return the child and we do not need to call init_fn.
  {
    auto it = children.Read()->find(name);
    if (it != children.Read()->end()) {
      auto child = it.CopyPointer().Lock();
      if (child) {
        child->node->fs()->did_access_dir_entry(child);
        return fit::ok(std::make_pair(child, true));
      }
    }
  }

  auto result = lock_children().get_or_create_child(current_task, mount, name, create_node_fn);
  if (result.is_error()) {
    return result.take_error();
  }

  auto [child, exists] = result.value();
  child->node->fs()->purge_old_entries();
  return fit::ok(std::make_pair(child, exists));
}
#endif

fbl::Vector<FsString> DirEntry::copy_child_names() {
  fbl::Vector<FsString> child_names;
  auto c = children.Read();
  for (auto iter = c->begin(); iter != c->end(); ++iter) {
    if (iter.IsValid()) {
      auto child = iter.CopyPointer().Lock();
      if (child) {
        fbl::AllocChecker ac;
        child_names.push_back(child->local_name(), &ac);
        if (!ac.check())
          break;
      }
    }
  }
  return child_names;
}

#if 0
fit::result<Errno, std::pair<DirEntryHandle, bool>> DirEntryLockedChildren::get_or_create_child(
    const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
    std::function<fit::result<Errno, FsNodeHandle>(const FsNodeHandle&, const MountInfo&,
                                                   const FsStr&)>
        create_fn) {
  LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.length()), name.data());

  auto create_child = [&]() -> fit::result<Errno, std::pair<DirEntryHandle, bool>> {
    auto find_or_create_node = [&]() -> fit::result<Errno, std::pair<FsNodeHandle, bool>> {
      if (auto result = entry_->node->lookup(current_task, mount, name); result.is_error()) {
        if (result.error_value().error_code() == ENOENT) {
          if (auto node = create_fn(entry_->node, mount, name); node.is_error()) {
            return node.take_error();
          } else {
            return fit::ok(std::make_pair(node.value(), false));
          }
        } else {
          return result.take_error();
        }
      } else {
        auto node = result.value();
        return fit::ok(std::make_pair(node, true));
      }
    }();

    if (find_or_create_node.is_error())
      return find_or_create_node.take_error();

    auto [node, exists] = find_or_create_node.value();

    ASSERT_MSG((node->info()->mode & FileMode::IFMT) != FileMode::EMPTY,
               "FsNode initialization did not populate the FileMode in FsNodeInfo.");

    auto entry = DirEntry::New(node, {entry_}, name);
    return fit::ok(std::make_pair(entry, exists));
  };

  auto it = children_->find(name);
  auto result = [&]() -> fit::result<Errno, std::pair<DirEntryHandle, bool>> {
    if (it == children_->end()) {
      // Vacant
      if (auto result = create_child(); result.is_error()) {
        return result.take_error();
      } else {
        auto [child, exists] = result.value();
        children_->insert(util::WeakPtr(child.get()));
        return fit::ok(std::make_pair(child, exists));
      }
    } else {
      // Occupied
      // It's possible that the upgrade will succeed this time around because we dropped
      // the read lock before acquiring the write lock. Another thread might have
      // populated this entry while we were not holding any locks.
      auto child = it.CopyPointer().Lock();
      if (child) {
        child->node->fs()->did_access_dir_entry(child);
        return fit::ok(std::make_pair(child, true));
      }

      if (auto result = create_child(); result.is_error()) {
        return result.take_error();
      } else {
        auto [new_child, exists] = result.value();
        children_->insert(util::WeakPtr(new_child.get()));
        return fit::ok(std::make_pair(new_child, exists));
      }
    }
  }();

  if (result.is_error()) {
    return result.take_error();
  }

  auto [child, exist] = result.value();
  child->node->fs()->did_create_dir_entry(child);
  return fit::ok(std::make_pair(child, exist));
}
#endif

}  // namespace starnix
