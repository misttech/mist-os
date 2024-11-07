// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_FS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_FS_H_

#include <lib/mistos/starnix/kernel/device/kobject.h>
#include <lib/mistos/starnix/kernel/vfs/symlink_node.h>
#include <lib/mistos/starnix_uapi/auth.h>

namespace starnix {

constexpr const char* SYSFS_DEVICES = "devices";
constexpr const char* SYSFS_BUS = "bus";
constexpr const char* SYSFS_CLASS = "class";
constexpr const char* SYSFS_BLOCK = "block";
constexpr const char* SYSFS_DEV = "dev";

/// Creates a path to the `to` kobject in the devices tree, relative to the `from` kobject from
/// a subsystem.
ktl::pair<ktl::unique_ptr<FsNodeOps>, std::function<FsNodeInfo(ino_t)>> sysfs_create_link(
    KObjectHandle from, KObjectHandle to, starnix_uapi::FsCred owner);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_SYSFS_FS_H_
