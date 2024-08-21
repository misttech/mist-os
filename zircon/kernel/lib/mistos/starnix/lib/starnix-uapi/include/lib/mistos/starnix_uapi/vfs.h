// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_VFS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_VFS_H_

#include <lib/mistos/util/bitflags.h>
#include <zircon/types.h>

// clang-format off
#include <asm-generic/statfs.h>
#include <linux/openat2.h>
// clang-format on

namespace starnix_uapi {

struct statfs default_statfs(uint32_t magic);

enum class ResolveFlagsEnum : uint32_t {
  NO_XDEV = RESOLVE_NO_XDEV,
  NO_MAGICLINKS = RESOLVE_NO_MAGICLINKS,
  NO_SYMLINKS = RESOLVE_NO_SYMLINKS,
  BENEATH = RESOLVE_BENEATH,
  IN_ROOT = RESOLVE_IN_ROOT,
  CACHED = RESOLVE_CACHED,
};

using ResolveFlags = Flags<ResolveFlagsEnum>;

}  // namespace starnix_uapi

template <>
constexpr Flag<starnix_uapi::ResolveFlagsEnum> Flags<starnix_uapi::ResolveFlagsEnum>::FLAGS[] = {
    {starnix_uapi::ResolveFlagsEnum::NO_XDEV},     {starnix_uapi::ResolveFlagsEnum::NO_MAGICLINKS},
    {starnix_uapi::ResolveFlagsEnum::NO_SYMLINKS}, {starnix_uapi::ResolveFlagsEnum::BENEATH},
    {starnix_uapi::ResolveFlagsEnum::IN_ROOT},     {starnix_uapi::ResolveFlagsEnum::CACHED},
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_VFS_H_
