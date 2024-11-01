// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/simple_directory.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <trace.h>

#include <fbl/alloc_checker.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<Errno> SimpleDirectory::add_entry(FsStr name, FsNodeHandle entry, bool overwrite) {
  LTRACEF("this(%p) name=[%.*s]\n", this, static_cast<int>(name.length()), name.data());
  if (!overwrite && entries_.contains(name)) {
    return fit::error(errno(EROFS));
  }
  auto [it, inserted] = entries_.emplace(name, entry);
  if (!inserted) {
    return fit::error(errno(EROFS));
  }
  return fit::ok();
}

fit::result<Errno, ktl::unique_ptr<FileOps>> SimpleDirectory::create_file_ops(
    const FsNode& node, const CurrentTask& current_task, OpenFlags flags) const {
  return fit::error(errno(ENOSYS));
}

fit::result<Errno, FsNodeHandle> SimpleDirectory::lookup(const FsNode& node,
                                                         const CurrentTask& current_task,
                                                         const FsStr& name) const {
  LTRACEF("this(%p) name=[%.*s]\n", this, static_cast<int>(name.length()), name.data());
  auto it = entries_.find(name);
  if (it != entries_.end()) {
    return fit::ok(it->second);
  }
  return fit::error(errno(ENOENT));
}

}  // namespace starnix
