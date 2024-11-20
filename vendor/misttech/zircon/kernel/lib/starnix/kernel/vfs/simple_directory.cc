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

namespace starnix {

fit::result<Errno> SimpleDirectory::add_entry(FsStr name, FsNodeHandle entry, bool overwrite) {
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
  auto it = entries_.find(name);
  if (it != entries_.end()) {
    return fit::ok(it->second);
  }

  auto keys_str = [this]() {
    BString entries_str = "[";
    bool first = true;
    for (const auto& entry : entries_) {
      if (!first) {
        entries_str =
            mtl::format("%.*s, ", static_cast<int>(entries_str.size()), entries_str.data());
      }

      entries_str =
          mtl::format("%.*s%.*s", static_cast<int>(entries_str.size()), entries_str.data(),
                      static_cast<int>(entry.first.size()), entry.first.data());
      first = false;
    }
    return mtl::format("%.*s]", static_cast<int>(entries_str.length()), entries_str.data());
  }();

  return fit::error(errno(
      ENOENT, mtl::format("looking for %.*s in %.*s", static_cast<int>(name.length()), name.data(),
                          static_cast<int>(keys_str.length()), keys_str.data())));
}

}  // namespace starnix
