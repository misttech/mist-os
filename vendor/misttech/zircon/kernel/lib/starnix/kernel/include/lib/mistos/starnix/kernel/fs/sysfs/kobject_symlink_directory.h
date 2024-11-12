// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_KOBJECT_SYMLINK_DIRECTORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_KOBJECT_SYMLINK_DIRECTORY_H_

#include <lib/mistos/starnix/kernel/device/kobject.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/fs.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/vec_directory.h>
#include <lib/mistos/memory/weak_ptr.h>

namespace starnix {

class KObjectSymlinkDirectory : public FsNodeOps {
 public:
  static KObjectSymlinkDirectory* New(const mtl::WeakPtr<KObject>& kobject) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) KObjectSymlinkDirectory(kobject);
    ZX_ASSERT(ac.check());
    return ptr;
  }

 private:
  KObjectHandle kobject() const {
    auto obj = kobject_.Lock();
    ZX_ASSERT_MSG(obj, "Weak references to kobject must always be valid");
    return obj;
  }

 public:
  // impl FsNodeOps for KObjectSymlinkDirectory
  fs_node_impl_dir_readonly();

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const final {
    fbl::AllocChecker ac;
    fbl::Vector<VecDirectoryEntry> entries;
    for (const auto& name : kobject()->get_children_names()) {
      entries.push_back(
          VecDirectoryEntry{
              .entry_type = DirectoryEntryType::LNK, .name = name, .inode = ktl::nullopt},
          &ac);
      ZX_ASSERT(ac.check());
    }
    return fit::ok(VecDirectory::new_file(ktl::move(entries)));
  }

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) const final {
    auto kobject = this->kobject();
    if (auto child = kobject->get_child(name)) {
      auto [link, info] = sysfs_create_link(kobject, *child, FsCred::root());
      return fit::ok(node.fs()->create_node(current_task, ktl::move(link), info));
    }
    return fit::error(errno(ENOENT));
  }

 private:
  explicit KObjectSymlinkDirectory(const mtl::WeakPtr<KObject>& kobject) : kobject_(kobject) {}

  mtl::WeakPtr<KObject> kobject_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_KOBJECT_SYMLINK_DIRECTORY_H_
