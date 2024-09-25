// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_TIME_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_TIME_H_

#include <stdint.h>

namespace starnix_uapi {

// Frequence of the "scheduler clock", which is used to report time values in some APIs, e.g. in
// `/proc` and `times()`. The same clock may be referred to as "USER_HZ" or "clock ticks".
// Passed to Linux processes by the loader as `AT_CLKTCK`, which they can get it from libc using
// `sysconf(_SC_CLK_TCK)`. Linux usually uses 100Hz.
const uint64_t SCHEDULER_CLOCK_HZ = 100;

}  // namespace starnix_uapi

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_TIME_H_
