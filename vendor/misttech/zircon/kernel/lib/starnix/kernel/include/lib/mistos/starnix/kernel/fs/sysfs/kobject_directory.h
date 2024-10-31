// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_KOBJECT_DIRECTORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_KOBJECT_DIRECTORY_H_

#include <lib/mistos/starnix/kernel/device/kobject.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/vec_directory.h>
#include <lib/mistos/util/weak_wrapper.h>

namespace starnix {

class KObjectDirectory : public FsNodeOps {
 public:
  static KObjectDirectory* New(const util::WeakPtr<KObject>& kobject) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) KObjectDirectory(kobject);
    ZX_ASSERT(ac.check());
    return ptr;
  }

  fs_node_impl_dir_readonly();

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) final {
    fbl::AllocChecker ac;
    fbl::Vector<VecDirectoryEntry> entries;
    for (const auto& name : kobject()->get_children_names()) {
      entries.push_back(
          VecDirectoryEntry{
              .entry_type = DirectoryEntryType::DIR, .name = name, .inode = ktl::nullopt},
          &ac);
      ZX_ASSERT(ac.check());
    }
    return fit::ok(VecDirectory::new_file(ktl::move(entries)));
  }

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) final {
    if (auto child = kobject()->get_child(name)) {
      return fit::ok(
          node.fs()->create_node(current_task, ktl::move((*child)->ops()),
                                 FsNodeInfo::new_factory(FILE_MODE(IFDIR, 0755), FsCred::root())));
    }
    return fit::error(errno(ENOENT));
  }

 private:
  explicit KObjectDirectory(const util::WeakPtr<KObject>& kobject) : kobject_(kobject) {}

  KObjectHandle kobject() const {
    auto obj = kobject_.Lock();
    ZX_ASSERT_MSG(obj, "Weak references to kobject must always be valid");
    return obj;
  }

  util::WeakPtr<KObject> kobject_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_KOBJECT_DIRECTORY_H_
