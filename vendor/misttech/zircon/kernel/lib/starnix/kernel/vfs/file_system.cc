// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/file_system.h"

#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_system_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <trace.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/unique_ptr.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

Entries Entries::Permanent(ktl::unique_ptr<Entries::Inner::Permanent> perm) {
  return Entries(ktl::move(perm));
}
Entries Entries::Lru(ktl::unique_ptr<Entries::Inner::LruCache> lru) {
  return Entries(ktl::move(lru));
}
Entries Entries::None() { return Entries(Variant()); }
Entries::Entries(Variant entries) : entries_(ktl::move(entries)) {}
Entries::Entries(Entries&& other) = default;
Entries& Entries::operator=(Entries&& other) = default;
Entries::~Entries() = default;

FileSystem::~FileSystem() {
  LTRACE_ENTRY_OBJ;
  ops_->unmount();
  // nodes_.Lock()->clear();
  LTRACE_EXIT_OBJ;
}

fit::result<Errno, FileSystemHandle> FileSystem::New(const fbl::RefPtr<Kernel>& kernel,
                                                     CacheMode cache_mode, FileSystemOps* ops,
                                                     FileSystemOptions options) {
  // let security_state = security::file_system_init_security(ops.name(), &options.params)?;

  auto cache = [&cache_mode]() -> Entries {
    fbl::AllocChecker ac;
    switch (cache_mode.type) {
      case CacheMode::Type::Permanent: {
        auto ptr = ktl::make_unique<Entries::Inner::Permanent>(&ac);
        ZX_ASSERT(ac.check());
        return Entries::Permanent(ktl::move(ptr));
      }
      case CacheMode::Type::Cached: {
        auto ptr = ktl::make_unique<Entries::Inner::LruCache>(&ac, cache_mode.config.capacity);
        ZX_ASSERT(ac.check());
        return Entries::Lru(ktl::move(ptr));
      }
      case CacheMode::Type::Uncached:
        return Entries::None();
    };
  };

  fbl::AllocChecker ac;
  auto file_system = fbl::AdoptRef(new (&ac) FileSystem(kernel, ktl::unique_ptr<FileSystemOps>(ops),
                                                        ktl::move(options), ktl::move(cache())));
  ZX_ASSERT(ac.check());

  // TODO: https://fxbug.dev/366405587 - Workaround to allow SELinux to note that this
  // `FileSystem` needs labeling, once a policy has been loaded.
  // security::file_system_post_init_security(kernel, &file_system);

  return fit::ok(ktl::move(file_system));
}

FileSystem::FileSystem(const fbl::RefPtr<Kernel>& kernel, ktl::unique_ptr<FileSystemOps> ops,
                       FileSystemOptions options, Entries entries)
    : kernel_(kernel->weak_factory_.GetWeakPtr()),
      next_node_id_(1),
      ops_(ktl::move(ops)),
      options_(ktl::move(options)),
      dev_id_(kernel->device_registry_.next_anonymous_dev_id()),
      entries_(ktl::move(entries)),
      weak_factory_(this) {}

ino_t FileSystem::next_node_id() const {
  ZX_ASSERT(!ops_->generate_node_ids());
  return next_node_id_.fetch_add(1, ktl::memory_order_relaxed);
}

fit::result<Errno> FileSystem::rename(const CurrentTask& current_task,
                                      const FsNodeHandle& old_parent, const FsString& old_name,
                                      const FsNodeHandle& new_parent, const FsString& new_name,
                                      const FsNodeHandle& renamed,
                                      ktl::optional<FsNodeHandle> replaced) const {
  // auto locked = starnix_sync::Locked<FileOpsCore>::from(mount);
  return ops_->rename(*this, current_task, old_parent, old_name, new_parent, new_name, renamed,
                      replaced);
}

fit::result<Errno> FileSystem::exchange(const CurrentTask& current_task, const FsNodeHandle& node1,
                                        const FsNodeHandle& parent1, const FsStr& name1,
                                        const FsNodeHandle& node2, const FsNodeHandle& parent2,
                                        const FsStr& name2) const {
  return ops_->exchange(*this, current_task, node1, parent1, name1, node2, parent2, name2);
}

fit::result<Errno, struct ::statfs> FileSystem::statfs(const CurrentTask& current_task) const {
  // Check security permissions first
  // security::sb_statfs(current_task, &self)?;
  auto result = ops_->statfs(*this, current_task) _EP(result);
  struct ::statfs stat = result.value();
  if (stat.f_frsize == 0) {
    stat.f_frsize = stat.f_bsize;
  }
  return fit::ok(stat);
}

void FileSystem::set_root(FsNodeOps* root) { set_root_node(FsNode::new_root(root)); }

// Set up the root of the filesystem. Must not be called more than once.
void FileSystem::set_root_node(FsNode* root) {
  auto root_handle = insert_node(root);
  ZX_ASSERT_MSG(root_.set(root_handle), "FileSystem::set_root can't be called more than once");
}

DirEntryHandle FileSystem::insert_node(FsNode* node) {
  fbl::RefPtr<FileSystem> self(this);

  if (node->node_id_ == 0) {
    node->set_id(next_node_id());
  }
  node->set_fs(self);
  FsNodeHandle handle = node->into_handle();
  self->nodes_.Lock()->insert(ktl::move(handle->weak_factory_.GetWeakPtr()));
  return DirEntry::New(handle, {}, FsString());
}

bool FileSystem::has_permanent_entries() const { return false; }

WeakFsNodeHandle FileSystem::prepare_node_for_insertion(const CurrentTask& current_task,
                                                        const FsNodeHandle& node) {
  /*
    if let Some(label) = self.selinux_context.get() {
        let _ = node.ops().set_xattr(
            node,
            current_task,
            "security.selinux".into(),
            label.as_ref(),
            XattrOp::Create,
        );
    }
  */
  return node->weak_factory_.GetWeakPtr();
}

FsNodeHandle FileSystem::create_node_with_id(const CurrentTask& current_task,
                                             ktl::unique_ptr<FsNodeOps> ops, ino_t id,
                                             FsNodeInfo info) {
  auto node =
      FsNode::new_uncached(current_task, ktl::move(ops), fbl::RefPtr<FileSystem>(this), id, info);
  nodes_.Lock()->insert(prepare_node_for_insertion(current_task, node));
  return node;
}

FsNodeHandle FileSystem::create_node_with_id_and_creds(
    ktl::unique_ptr<FsNodeOps> ops, ino_t id, FsNodeInfo info,
    const starnix_uapi::Credentials& credentials) {
  auto node = FsNode::new_uncached_with_creds(ktl::move(ops), fbl::RefPtr<FileSystem>(this), id,
                                              info, credentials);
  nodes_.Lock()->insert(node->weak_factory_.GetWeakPtr());
  return node;
}

void FileSystem::remove_node(const FsNode& node) {
  auto nodes = nodes_.Lock();
  if (auto weak_node = nodes->find(node.node_id_); weak_node != nodes->end()) {
    // If strong count is zero (Lock will fail), means it was alread destroyed.
    if (!weak_node.CopyPointer().Lock()) {
      nodes->erase(node.node_id_);
    }
  }
}

void FileSystem::did_create_dir_entry(const DirEntryHandle& entry) {
  ktl::visit(Entries::overloaded{[](const ktl::monostate&) {},
                                 [&entry](const ktl::unique_ptr<Entries::Inner::Permanent>& p) {
                                   p->entries.Lock()->insert_or_find(entry);
                                 },
                                 [&entry](const ktl::unique_ptr<Entries::Inner::LruCache>& c) {
                                   c->entries.Lock()->insert_or_find(entry);
                                 }},
             entries_.entries_);
}

void FileSystem::will_destroy_dir_entry(const DirEntryHandle& entry) {
  ktl::visit(Entries::overloaded{[](const ktl::monostate&) {},
                                 [&entry](const ktl::unique_ptr<Entries::Inner::Permanent>& p) {
                                   p->entries.Lock()->erase(entry->GetKey());
                                 },
                                 [&entry](const ktl::unique_ptr<Entries::Inner::LruCache>& c) {
                                   c->entries.Lock()->erase(entry->GetKey());
                                 }},
             entries_.entries_);
}

void FileSystem::did_access_dir_entry(const DirEntryHandle& entry) {
  ktl::visit(Entries::overloaded{[](const ktl::monostate&) {},
                                 [](const ktl::unique_ptr<Entries::Inner::Permanent>&) {},
                                 [&entry](const ktl::unique_ptr<Entries::Inner::LruCache>& c) {
                                   auto entries = c->entries.Lock();
                                   if (auto it = entries->find(entry->GetKey());
                                       it != entries->end()) {
                                     // Move to end to mark as most recently used
                                     auto node = entries->erase(it);
                                     entries->insert(node);
                                   }
                                 }},
             entries_.entries_);
}

void FileSystem::purge_old_entries() {
  ktl::visit(Entries::overloaded{[](const ktl::monostate&) {},
                                 [](const ktl::unique_ptr<Entries::Inner::Permanent>&) {},
                                 [](const ktl::unique_ptr<Entries::Inner::LruCache>& c) {
                                   fbl::AllocChecker ac;
                                   fbl::Vector<DirEntryHandle> purged;
                                   {
                                     auto entries = c->entries.Lock();
                                     while (entries->size() > c->capacity) {
                                       auto it = entries->begin();
                                       purged.push_back(it.CopyPointer(), &ac);
                                       ZX_ASSERT(ac.check());
                                       entries->erase(it);
                                     }
                                   }
                                   // Entries will get dropped here while not holding the lock
                                 }},
             entries_.entries_);
}

FsStr FileSystem::name() const { return ops_->name(); }

DirEntryHandle FileSystem::root() {
  auto root = root_.get();
  ZX_ASSERT_MSG(root, "FileSystem %.*s has no root", static_cast<int>(name().size()),
                name().data());
  return root;
}

}  // namespace starnix
