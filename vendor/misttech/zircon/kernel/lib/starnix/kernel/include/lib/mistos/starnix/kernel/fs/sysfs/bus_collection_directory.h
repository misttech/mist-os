// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_BUS_COLLECTION_DIRECTORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_BUS_COLLECTION_DIRECTORY_H_

#include <lib/mistos/starnix/kernel/device/kobject.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/fs.h>
#include <lib/mistos/starnix/kernel/vfs/vec_directory.h>
#include <lib/mistos/memory/weak_ptr.h>

namespace starnix {

class BusDevicesDirectory : public FsNodeOps {
 private:
  mtl::WeakPtr<KObject> kobject_;

 public:
  static BusDevicesDirectory* New(mtl::WeakPtr<KObject> kobject) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) BusDevicesDirectory(ktl::move(kobject));
    if (!ac.check()) {
      return nullptr;
    }
    return ptr;
  }

 private:
  KObjectHandle kobject() const {
    auto ptr = kobject_.Lock();
    ASSERT_MSG(ptr, "Weak references to kobject must always be valid");
    return ptr;
  }

  // impl FsNodeOps for BusDevicesDirectory
  fs_node_impl_dir_readonly();

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const override {
    fbl::AllocChecker ac;
    fbl::Vector<VecDirectoryEntry> entries;

    auto kobj = kobject();
    auto names = kobj->get_children_names();
    for (const auto& name : names) {
      entries.push_back(
          {
              VecDirectoryEntry{
                  .entry_type = DirectoryEntryType::LNK,
                  .name = name,
                  .inode = ktl::nullopt,
              },
          },
          &ac);
      if (!ac.check()) {
        return fit::error(errno(ENOMEM));
      }
    }

    return fit::ok(VecDirectory::new_file(ktl::move(entries)));
  }

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) const override {
    auto kobj = kobject();
    auto child = kobj->get_child(name);
    if (!child) {
      return fit::error(errno(ENOENT));
    }

    auto [link, info] = sysfs_create_link(kobj, child.value(), FsCred::root());
    return fit::ok(node.fs()->create_node(current_task, ktl::move(link), info));
  }

 private:
  explicit BusDevicesDirectory(mtl::WeakPtr<KObject> kobject) : kobject_(ktl::move(kobject)) {}
};

class BusCollectionDirectory : public FsNodeOps {
 private:
  mtl::WeakPtr<KObject> kobject_;

 public:
  static BusCollectionDirectory* New(mtl::WeakPtr<KObject> kobject) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) BusCollectionDirectory(ktl::move(kobject));
    if (!ac.check()) {
      return nullptr;
    }
    return ptr;
  }

 private:
  // impl FsNodeOps for BusCollectionDirectory
  fs_node_impl_dir_readonly();

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const override {
    fbl::AllocChecker ac;
    fbl::Vector<VecDirectoryEntry> entries;
    entries.push_back(
        {
            VecDirectoryEntry{
                .entry_type = DirectoryEntryType::DIR,
                .name = FsString("devices"),
                .inode = ktl::nullopt,
            },
        },
        &ac);
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }
    return fit::ok(VecDirectory::new_file(ktl::move(entries)));
  }

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) const override {
    if (name == "devices") {
      return fit::ok(node.fs()->create_node(
          current_task, ktl::unique_ptr<FsNodeOps>(BusDevicesDirectory::New(kobject_)),
          FsNodeInfo::new_factory(FILE_MODE(IFDIR, 0755), FsCred::root())));
    }
    return fit::error(errno(ENOENT));
  }

 private:
  explicit BusCollectionDirectory(mtl::WeakPtr<KObject> kobject) : kobject_(ktl::move(kobject)) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_BUS_COLLECTION_DIRECTORY_H_
