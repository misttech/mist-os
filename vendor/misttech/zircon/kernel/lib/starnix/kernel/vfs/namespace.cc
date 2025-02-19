// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/namespace.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <trace.h>

#include <fbl/ref_ptr.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

namespace {

// A file that represents a mount namespace.
class MountNamespaceFile : public FileOps {
 public:
  // impl FileOps for MountNamespaceFile
  fileops_impl_nonseekable();
  fileops_impl_dataless();
  fileops_impl_noop_sync();

  explicit MountNamespaceFile(fbl::RefPtr<Namespace> ns) : namespace_(ktl::move(ns)) {}

 private:
  fbl::RefPtr<Namespace> namespace_;
};

}  // namespace

fbl::RefPtr<Namespace> Namespace::New(FileSystemHandle fs) {
  return Namespace::new_with_flags(fs, MountFlags::empty());
}

fbl::RefPtr<Namespace> Namespace::new_with_flags(FileSystemHandle fs, MountFlags flags) {
  auto kernel = fs->kernel_.Lock();
  ASSERT_MSG(kernel, "can't create namespace without a kernel");
  auto root_mount = Mount::New(WhatToMount::Fs(fs), MountFlags::empty());
  fbl::AllocChecker ac;
  auto handle = fbl::AdoptRef(new (&ac) Namespace(root_mount, kernel->get_next_namespace_id()));
  ZX_ASSERT(ac.check());
  return handle;
}

NamespaceNode Namespace::root() { return root_mount_->root(); }

fbl::RefPtr<Namespace> Namespace::clone_namespace() const {
  auto kernel = root_mount_->fs_->kernel_.Lock();
  ASSERT_MSG(kernel, "can't clone namespace without a kernel");
  fbl::AllocChecker ac;
  auto handle = fbl::AdoptRef(
      new (&ac) Namespace(root_mount_->clone_mount_recursive(), kernel->get_next_namespace_id()));
  ZX_ASSERT(ac.check());
  return handle;
}

fit::result<Errno, ktl::unique_ptr<FileOps>> Namespace::create_file_ops(
    const FsNode& node, const CurrentTask& current_task, OpenFlags flags) const {
  fbl::AllocChecker ac;
  auto ptr = new (&ac) MountNamespaceFile(fbl::RefPtr<Namespace>(const_cast<Namespace*>(this)));
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }
  return fit::ok(ktl::unique_ptr<MountNamespaceFile>(ptr));
}

Namespace::Namespace(MountHandle root_mount, uint64_t id)
    : root_mount_(ktl::move(root_mount)), id_(id) {
  LTRACEF_LEVEL(2, "id=%lu\n", id_);
}

Namespace::~Namespace() { LTRACE_ENTRY_OBJ; }

LookupContext LookupContext::New(SymlinkMode _symlink_mode) {
  return {.symlink_mode = _symlink_mode,
          .remaining_follows = MAX_SYMLINK_FOLLOWS,
          .must_be_directory = false,
          .resolve_flags = ResolveFlags::empty(),
          .resolve_base = ResolveBase::None()};
}

LookupContext LookupContext::with(SymlinkMode _symlink_mode) {
  LookupContext tmp = *this;
  tmp.symlink_mode = _symlink_mode;
  tmp.resolve_base = this->resolve_base;
  return ktl::move(tmp);
}

void LookupContext::update_for_path(const FsStr& path) {
  if (path[path.length()] == '/') {
    // The last path element must resolve to a directory. This is because a trailing slash
    // was found in the path.
    must_be_directory = true;
    // If the last path element is a symlink, we should follow it.
    // See https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
    symlink_mode = SymlinkMode::Follow;
  }
}

LookupContext LookupContext::Default() { return New(SymlinkMode::Follow); }

}  // namespace starnix
