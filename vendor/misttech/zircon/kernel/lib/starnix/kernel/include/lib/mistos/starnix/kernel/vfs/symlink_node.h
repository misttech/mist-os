// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYMLINK_NODE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYMLINK_NODE_H_

#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/kernel/vfs/xattr.h>

#include <utility>

namespace starnix {

// A node that represents a symlink to another node.
class SymlinkNode : public FsNodeOps {
 private:
  // The target of the symlink (the path to use to find the actual node).
  FsString target_;

  // Extended attributes storage for this symlink.
  MemoryXattrStorage xattrs_;

 public:
  // Creates a new symlink node with the given target path and owner credentials.
  static ktl::pair<ktl::unique_ptr<FsNodeOps>, std::function<FsNodeInfo(ino_t)>> New(
      const FsStr& target, FsCred owner) {
    auto size = target.size();
    auto info = [size, owner](ino_t ino) {
      auto info = FsNodeInfo::New(ino, FILE_MODE(IFLNK, 0777), owner);
      info.size = size;
      return info;
    };
    fbl::AllocChecker ac;
    auto ptr = new (&ac) SymlinkNode(target);
    ZX_ASSERT(ac.check());
    return ktl::pair(ktl::unique_ptr<FsNodeOps>(ptr), info);
  }

  // impl FsNodeOps for SymlinkNode
  fs_node_impl_symlink();
  fs_node_impl_xattr_delegate(xattrs_);

  // Reads the target path of this symlink.
  fit::result<Errno, SymlinkTarget> readlink(const FsNode& node,
                                             const CurrentTask& current_task) const override {
    return fit::ok(SymlinkTarget::Path(target_));
  }

 private:
  // Constructor for SymlinkNode
  explicit SymlinkNode(FsString target)
      : target_(std::move(target)), xattrs_(MemoryXattrStorage::Default()) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_SYMLINK_NODE_H_
