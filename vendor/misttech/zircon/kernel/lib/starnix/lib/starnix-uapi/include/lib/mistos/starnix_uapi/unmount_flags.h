// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_UNMOUNT_FLAGS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_UNMOUNT_FLAGS_H_

#include <lib/mistos/util/bitflags.h>

#include <linux/mount.h>

namespace starnix_uapi {

constexpr uint32_t MNT_FORCE = 1;
constexpr uint32_t MNT_DETACH = 2;
constexpr uint32_t MNT_EXPIRE = 4;
constexpr uint32_t UMOUNT_NOFOLLOW = 8;

enum class UnmountFlagsEnum : uint32_t {
  FORCE = MNT_FORCE,
  DETACH = MNT_DETACH,
  EXPIRE = MNT_EXPIRE,
  NOFOLLOW = UMOUNT_NOFOLLOW,
};

using UnmountFlags = Flags<UnmountFlagsEnum>;

}  // namespace starnix_uapi

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_UNMOUNT_FLAGS_H_
