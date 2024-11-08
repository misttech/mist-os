// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/file_system.h"

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
#include <lib/mistos/util/weak_wrapper.h>
#include <trace.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/unique_ptr.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

FileSystem::~FileSystem() {
  ops_->unmount();
  nodes_.Lock()->clear();
}

FileSystemHandle FileSystem::New(const fbl::RefPtr<Kernel>& kernel, CacheMode cache_mode,
                                 FileSystemOps* ops, FileSystemOptions options) {
  fbl::AllocChecker ac;
  Entries entries;
  switch (cache_mode.type) {
    case CacheModeType::Permanent:
      entries = ktl::make_unique<Permanent>(&ac);
      ZX_ASSERT(ac.check());
      break;
    case CacheModeType::Cached:
      entries = ktl::make_unique<LruCache>(&ac, cache_mode.config.capacity);
      ZX_ASSERT(ac.check());
      break;
    case CacheModeType::Uncached:
      break;
  };

  auto fs = fbl::AdoptRef(new (&ac) FileSystem(kernel, ktl::unique_ptr<FileSystemOps>(ops),
                                               ktl::move(options), ktl::move(entries)));
  ZX_ASSERT(ac.check());
  return ktl::move(fs);
}

FileSystem::FileSystem(const fbl::RefPtr<Kernel>& kernel, ktl::unique_ptr<FileSystemOps> ops,
                       FileSystemOptions options, Entries entries)
    : kernel_(kernel.get()),
      next_node_id_(1),
      ops_(ktl::move(ops)),
      options_(ktl::move(options)),
      dev_id_(kernel->device_registry_.next_anonymous_dev_id()),
      entries_(ktl::move(entries)) {}

ino_t FileSystem::next_node_id() const {
  ZX_ASSERT(!ops_->generate_node_ids());
  return next_node_id_.fetch_add(1, ktl::memory_order_relaxed);
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
  self->nodes_.Lock()->insert(util::WeakPtr(handle.get()));
  return DirEntry::New(handle, {}, FsString());
}

bool FileSystem::has_permanent_entries() const { return false; }

WeakFsNodeHandle FileSystem::prepare_node_for_insertion(/*const CurrentTask& current_task,*/
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
  return util::WeakPtr(node.get());
}

FsNodeHandle FileSystem::create_node_with_id(const CurrentTask& current_task,
                                             ktl::unique_ptr<FsNodeOps> ops, ino_t id,
                                             FsNodeInfo info) {
  auto creds = current_task->creds();
  return create_node_with_id(ktl::move(ops), id, info, creds);
}

FsNodeHandle FileSystem::create_node_with_id(ktl::unique_ptr<FsNodeOps> ops, ino_t id,
                                             FsNodeInfo info,
                                             const starnix_uapi::Credentials& credentials) {
  auto node =
      FsNode::new_uncached(ktl::move(ops), fbl::RefPtr<FileSystem>(this), id, info, credentials);

  nodes_.Lock()->insert(prepare_node_for_insertion(/*current_task,*/ node));

  return node;
}

/// Remove the given FsNode from the node cache.
///
/// Called from the Release trait of FsNode.
void FileSystem::remove_node(const FsNode& node) {}

void FileSystem::did_create_dir_entry(const DirEntryHandle& entry) { LTRACE; }

void FileSystem::will_destroy_dir_entry(const DirEntryHandle& entry) { LTRACE; }

void FileSystem::did_access_dir_entry(const DirEntryHandle& entry) { LTRACE; }

void FileSystem::purge_old_entries() { LTRACE; }

DirEntryHandle FileSystem::root() { return root_.get(); }

}  // namespace starnix
