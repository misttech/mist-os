// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_context.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/task_wrapper.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix_uapi/file_mode.h>

#include <utility>

namespace starnix {

NamespaceNode FsContext::cwd() const {
  Guard<Mutex> lock(&fs_state_rw_lock_);
  return state_.cwd;
}

NamespaceNode FsContext::root() const {
  Guard<Mutex> lock(&fs_state_rw_lock_);
  return state_.root;
}

FileMode FsContext::umask() const {
  Guard<Mutex> lock(&fs_state_rw_lock_);
  return state_.umask;
}

FileMode FsContext::apply_umask(FileMode mode) const {
  Guard<Mutex> lock(&fs_state_rw_lock_);
  return mode & !state_.umask;
}

FileMode FsContext::set_umask(FileMode umask) const {
  Guard<Mutex> lock(&fs_state_rw_lock_);
  auto old_umask = state_.umask;

  // umask() sets the calling process's file mode creation mask
  // (umask) to mask & 0o777 (i.e., only the file permission bits of
  // mask are used), and returns the previous value of the mask.
  //
  // See <https://man7.org/linux/man-pages/man2/umask.2.html>
  //state_.umask = umask & FileMode::from_bits(0777);

  return old_umask;
}

fbl::RefPtr<FsContext> FsContext::New(FileSystemHandle root) {
  auto ns = Namespace::New(root);
  auto root_ = ns->root();

  fbl::AllocChecker ac;
  auto handle =
      fbl::AdoptRef(new (&ac) FsContext({ns, root_, std::move(root_), FileMode::DEFAULT_UMASK}));
  ZX_ASSERT(ac.check());
  return handle;
}

FsContext::FsContext(FsContextState state) : state_(std::move(state)) {}

fbl::RefPtr<FsContext> FsContext::fork() const { return fbl::RefPtr<FsContext>(); }

}  // namespace starnix
