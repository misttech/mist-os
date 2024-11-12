// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_MOUNT_FLAGS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_MOUNT_FLAGS_H_

#include <lib/mistos/util/bitflags.h>

#include <linux/mount.h>

namespace starnix_uapi {

enum class MountFlagsEnum : uint32_t {
  // per-mountpoint flags
  RDONLY = MS_RDONLY,
  NOEXEC = MS_NOEXEC,
  NOSUID = MS_NOSUID,
  NODEV = MS_NODEV,
  NOATIME = MS_NOATIME,
  NODIRATIME = MS_NODIRATIME,
  RELATIME = MS_RELATIME,
  STRICTATIME = MS_STRICTATIME,

  // per-superblock flags
  SILENT = MS_SILENT,
  LAZYTIME = MS_LAZYTIME,
  SYNCHRONOUS = MS_SYNCHRONOUS,
  DIRSYNC = MS_DIRSYNC,
  MANDLOCK = MS_MANDLOCK,

  // mount() control flags
  REMOUNT = MS_REMOUNT,
  BIND = MS_BIND,
  REC = MS_REC,
  DOWNSTREAM = MS_SLAVE,
  SHARED = MS_SHARED,
  PRIVATE = MS_PRIVATE,

  // Flags stored in Mount state.
  STORED_ON_MOUNT = RDONLY | NOEXEC | NOSUID | NODEV | NOATIME | NODIRATIME | RELATIME,

  // Flags stored in FileSystem options.
  STORED_ON_FILESYSTEM = RDONLY | DIRSYNC | LAZYTIME | MANDLOCK | SILENT | SYNCHRONOUS,

  // Flags that change be changed with REMOUNT.
  //
  // MS_DIRSYNC and MS_SILENT cannot be changed with REMOUNT.
  CHANGEABLE_WITH_REMOUNT = STORED_ON_MOUNT | STRICTATIME | MANDLOCK | LAZYTIME | SYNCHRONOUS,
};

using MountFlags = Flags<MountFlagsEnum>;

}  // namespace starnix_uapi

template <>
constexpr Flag<starnix_uapi::MountFlagsEnum> Flags<starnix_uapi::MountFlagsEnum>::FLAGS[] = {
    {starnix_uapi::MountFlagsEnum::RDONLY},      {starnix_uapi::MountFlagsEnum::NOEXEC},
    {starnix_uapi::MountFlagsEnum::NOSUID},      {starnix_uapi::MountFlagsEnum::NODEV},
    {starnix_uapi::MountFlagsEnum::NOATIME},     {starnix_uapi::MountFlagsEnum::NODIRATIME},
    {starnix_uapi::MountFlagsEnum::RELATIME},    {starnix_uapi::MountFlagsEnum::STRICTATIME},
    {starnix_uapi::MountFlagsEnum::SILENT},      {starnix_uapi::MountFlagsEnum::LAZYTIME},
    {starnix_uapi::MountFlagsEnum::SYNCHRONOUS}, {starnix_uapi::MountFlagsEnum::DIRSYNC},
    {starnix_uapi::MountFlagsEnum::MANDLOCK},    {starnix_uapi::MountFlagsEnum::REMOUNT},
    {starnix_uapi::MountFlagsEnum::BIND},        {starnix_uapi::MountFlagsEnum::REC},
    {starnix_uapi::MountFlagsEnum::DOWNSTREAM},  {starnix_uapi::MountFlagsEnum::SHARED},
    {starnix_uapi::MountFlagsEnum::PRIVATE},
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_MOUNT_FLAGS_H_
