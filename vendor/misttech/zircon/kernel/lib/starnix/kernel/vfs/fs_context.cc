// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_context.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix_uapi/file_mode.h>

#include <ktl/enforce.h>

#include <linux/capability.h>

namespace starnix {

NamespaceNode FsContext::cwd() const {
  auto state = state_.Read();
  return state->cwd_.to_passive();
}

NamespaceNode FsContext::root() const {
  auto state = state_.Read();
  return state->root_.to_passive();
}

fit::result<Errno> FsContext::chdir(const CurrentTask& current_task,
                                    const NamespaceNode& name) const {
  auto result = name.check_access(current_task, Access(starnix_uapi::AccessEnum::EXEC),
                                  CheckAccessReason::Chdir);
  if (result.is_error()) {
    return result;
  }

  auto state = state_.Write();
  state->cwd_ = name.into_active();
  return fit::ok();
}

fit::result<Errno> FsContext::chroot(const CurrentTask& current_task,
                                     const NamespaceNode& name) const {
  auto access_result = name.check_access(current_task, Access(starnix_uapi::AccessEnum::EXEC),
                                         CheckAccessReason::Chroot);
  if (access_result.is_error()) {
    return fit::error(errno(EACCES));
  }
  if (!current_task->creds().has_capability(starnix_uapi::kCapSysChroot)) {
    return fit::error(errno(EPERM));
  }

  auto state = state_.Write();
  state->root_ = name.into_active();
  return fit::ok();
}

FileMode FsContext::umask() const { return state_.Read()->umask_; }

FileMode FsContext::apply_umask(FileMode mode) const {
  auto umask = state_.Read()->umask_;
  return mode & ~umask;
}

FileMode FsContext::set_umask(FileMode umask) const {
  auto state = state_.Write();
  auto old_umask = state->umask_;

  // umask() sets the calling process's file mode creation mask
  // (umask) to mask & 0o777 (i.e., only the file permission bits of
  // mask are used), and returns the previous value of the mask.
  //
  // See <https://man7.org/linux/man-pages/man2/umask.2.html>
  state->umask_ = umask & FileMode::from_bits(0777);

  return old_umask;
}

fbl::RefPtr<FsContext> FsContext::New(fbl::RefPtr<Namespace> _namespace) {
  auto root = _namespace->root();
  fbl::AllocChecker ac;
  auto handle =
      fbl::AdoptRef(new (&ac) FsContext(FsContextState{.namespace_ = _namespace,
                                                       .root_ = root.into_active(),
                                                       .cwd_ = root.into_active(),
                                                       .umask_ = FileMode::DEFAULT_UMASK}));
  ZX_ASSERT(ac.check());
  return handle;
}

FsContext::~FsContext() = default;

FsContext::FsContext(FsContextState state) : state_(ktl::move(state)) {}

fbl::RefPtr<FsContext> FsContext::fork() const {
  // A child process created via fork(2) inherits its parent's umask.
  // The umask is left unchanged by execve(2).
  //
  // See <https://man7.org/linux/man-pages/man2/umask.2.html>

  fbl::AllocChecker ac;
  auto handle = fbl::AdoptRef(new (&ac) FsContext(*state_.Read()));
  ZX_ASSERT(ac.check());
  return handle;
}

}  // namespace starnix
