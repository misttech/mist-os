// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_FS_MISTOS_SIMPLE_DIRECTORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_FS_MISTOS_SIMPLE_DIRECTORY_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/allocator.h>
#include <lib/mistos/util/btree_map.h>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

namespace starnix {

using starnix_uapi::FileMode;
using starnix_uapi::FsCred;

class FileSystem;
class CurrentTask;
class FsNode;
using FsNodeHandle = fbl::RefPtr<FsNode>;

class SimpleDirectory : public FsNodeOps {
 private:
  util::BTreeMap<const FsStr, FsNodeHandle> entries_;

 public:
  /// Adds a child entry to this directory.
  fit::result<Errno> add_entry(FsStr name, FsNodeHandle, bool overwrite = false);

  SimpleDirectory() = default;

  /// impl FsNodeOps
  fs_node_impl_dir_readonly();

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const final;

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) const final;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_FS_MISTOS_SIMPLE_DIRECTORY_H_
