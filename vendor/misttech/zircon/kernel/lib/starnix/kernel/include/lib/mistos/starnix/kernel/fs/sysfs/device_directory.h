// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_DEVICE_DIRECTORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_DEVICE_DIRECTORY_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/device/kobject.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/vec_directory.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/btree_map.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/mistos/util/range-map.h>
#include <lib/starnix_sync/locks.h>

#include <ktl/unique_ptr.h>

namespace starnix {

class DeviceDirectory : public FsNodeOps {
 private:
  Device device_;

 public:
  // impl DeviceDirectory
  static DeviceDirectory* New(Device device) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) DeviceDirectory(ktl::move(device));
    if (!ac.check()) {
      return nullptr;
    }
    return ptr;
  }

 private:
  DeviceType device_type() const { return device_.metadata_.device_type_; }

  static fbl::Vector<VecDirectoryEntry> create_file_ops_entries() {
    // TODO(https://fxbug.dev/42072346): Add power and subsystem nodes.
    fbl::AllocChecker ac;
    fbl::Vector<VecDirectoryEntry> entries;

    entries.push_back(
        VecDirectoryEntry{
            .entry_type = DirectoryEntryType::REG,
            .name = FsString("dev"),
            .inode = ktl::nullopt,
        },
        &ac);
    ZX_ASSERT(ac.check());

    entries.push_back(
        VecDirectoryEntry{
            .entry_type = DirectoryEntryType::REG,
            .name = FsString("uevent"),
            .inode = ktl::nullopt,
        },
        &ac);
    ZX_ASSERT(ac.check());

    return entries;
  }

 public:
  KObjectHandle kobject() const { return device_.kobject(); }

 private:
  // impl FsNodeOps for DeviceDirectory
  fs_node_impl_dir_readonly();

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const override {
    return fit::ok(VecDirectory::new_file(DeviceDirectory::create_file_ops_entries()));
  }

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) const override {
    if (name == "dev") {
      // Create dev file with device type string
      /*auto content = FsString::Format("%d\n", device_type().value());
      return fit::ok(
          node->fs()->create_node(current_task, ktl::make_unique<BytesFile>(ktl::move(content)),
                                  FsNodeInfo::new_factory(mode(IFREG, 0444), FsCred::root())));*/
    } /*else if (name == "uevent") {
      // Create uevent file
      return fit::ok(
          node->fs()->create_node(current_task, ktl::make_unique<UEventFsNode>(device_.Clone()),
                                  FsNodeInfo::new_factory(mode(IFREG, 0644), FsCred::root())));
    }*/
    return fit::error(errno(ENOENT));
  }

 private:
  explicit DeviceDirectory(Device device) : device_(ktl::move(device)) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_DEVICE_DIRECTORY_H_
