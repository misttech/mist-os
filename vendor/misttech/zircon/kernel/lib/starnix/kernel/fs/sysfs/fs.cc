// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/fs/sysfs/fs.h"

#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>

namespace starnix {

ktl::pair<ktl::unique_ptr<FsNodeOps>, std::function<FsNodeInfo(ino_t)>> sysfs_create_link(
    KObjectHandle from, KObjectHandle to, starnix_uapi::FsCred owner) {
  auto path = PathBuilder::New();
  path.prepend_element(to->path());
  // Escape one more level from its subsystem to the root of sysfs.
  path.prepend_element("..");

  auto path_to_root = from->path_to_root();
  if (!path_to_root.empty()) {
    path.prepend_element(path_to_root);
  }

  // Build a symlink with the relative path.
  return SymlinkNode::New(path.build_relative(), owner);
}

}  // namespace starnix
