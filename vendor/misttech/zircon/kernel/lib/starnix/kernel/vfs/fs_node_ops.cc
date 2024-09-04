// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_node_ops.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/errors.h>

namespace starnix {

fit::result<Errno, FsNodeHandle> FsNodeOps::lookup(const FsNode& node,
                                                   const CurrentTask& current_task,
                                                   const FsStr& name) {
  return fit::error(errno(ENOENT));
}

fit::result<Errno, FsNodeHandle> FsNodeOps::mknod(/*FileOpsCore& locked,*/ const FsNode& node,
                                                  const CurrentTask& current_task,
                                                  const FsStr& name, FileMode mode, DeviceType dev,
                                                  FsCred owner) {
  return fit::error(errno(EROFS));
}

fit::result<Errno, FsNodeHandle> FsNodeOps::mkdir(const FsNode& node,
                                                  const CurrentTask& current_task,
                                                  const FsStr& name, FileMode mode, FsCred owner) {
  return fit::error(errno(EROFS));
}

fit::result<Errno, FsNodeHandle> FsNodeOps::create_symlink(const FsNode& node,
                                                           const CurrentTask& current_task,
                                                           const FsStr& name, const FsStr& target,
                                                           FsCred owner) {
  return fit::error(errno(EROFS));
}

fit::result<Errno, FsNodeHandle> FsNodeOps::create_tmpfile(const FsNode& node,
                                                           const CurrentTask& current_task,
                                                           FileMode mode, FsCred owner) {
  return fit::error(errno(EOPNOTSUPP));
}

}  // namespace starnix
