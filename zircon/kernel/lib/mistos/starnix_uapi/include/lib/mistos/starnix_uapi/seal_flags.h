// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_SEAL_FLAGS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_SEAL_FLAGS_H_

#include <lib/mistos/util/bitflags.h>

#include <linux/fcntl.h>

namespace starnix_uapi {

enum class SealFlagsEnum : uint32_t {
  FUTURE_WRITE = F_SEAL_FUTURE_WRITE,
  WRITE = F_SEAL_WRITE,
  GROW = F_SEAL_GROW,
  SHRINK = F_SEAL_SHRINK,
  SEAL = F_SEAL_SEAL,
};

using SealFlags = Flags<SealFlagsEnum>;

}  // namespace starnix_uapi

template <>
constexpr Flag<starnix_uapi::SealFlagsEnum> Flags<starnix_uapi::SealFlagsEnum>::FLAGS[] = {
    {starnix_uapi::SealFlagsEnum::FUTURE_WRITE}, {starnix_uapi::SealFlagsEnum::WRITE},
    {starnix_uapi::SealFlagsEnum::GROW},         {starnix_uapi::SealFlagsEnum::SHRINK},
    {starnix_uapi::SealFlagsEnum::SEAL},
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_SEAL_FLAGS_H_
