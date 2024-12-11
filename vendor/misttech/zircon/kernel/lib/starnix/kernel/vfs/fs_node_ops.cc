// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_node_ops.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/errors.h>

namespace starnix {

FsNodeOps::~FsNodeOps() = default;

fit::result<Errno> FsNodeOps::check_access(const FsNode& node, const CurrentTask& current_task,
                                           starnix_uapi::Access access,
                                           starnix_sync::RwLock<FsNodeInfo>& info,
                                           CheckAccessReason _reason) const {
  return starnix::FsNode::default_check_access_impl(current_task, access, info.Read());
}

fit::result<Errno, FsNodeHandle> FsNodeOps::lookup(const FsNode& node,
                                                   const CurrentTask& current_task,
                                                   const FsStr& name) const {
  // The default implementation here is suitable for filesystems that have permanent entries;
  // entries that already exist will get found in the cache and shouldn't get this far.
  return fit::error(
      errno(ENOENT, mtl::format("looking for %.*s", static_cast<int>(name.length()), name.data())));
}

fit::result<Errno, FsNodeHandle> FsNodeOps::create_tmpfile(const FsNode& node,
                                                           const CurrentTask& current_task,
                                                           FileMode mode, FsCred owner) const {
  return fit::error(errno(EOPNOTSUPP));
}

XattrStorage::~XattrStorage() = default;

}  // namespace starnix
