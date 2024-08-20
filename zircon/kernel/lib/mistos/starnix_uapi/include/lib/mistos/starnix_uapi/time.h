// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_TIME_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_TIME_H_

#include <zircon/types.h>

namespace starnix_uapi {

//namespace {
//const int64_t MICROS_PER_SECOND = 1000 * 1000;
//}

static const int64_t NANOS_PER_SECOND = 1000 * 1000 * 1000;

// Frequence of the "scheduler clock", which is used to report time values in some APIs, e.g. in
// `/proc` and `times()`. The same clock may be referred to as "USER_HZ" or "clock ticks".
// Passed to Linux processes by the loader as `AT_CLKTCK`, which they can get it from libc using
// `sysconf(_SC_CLK_TCK)`. Linux usually uses 100Hz.
static const int64_t SCHEDULER_CLOCK_HZ = 100;

// const_assert_eq!(NANOS_PER_SECOND % SCHEDULER_CLOCK_HZ, 0);
static const int64_t NANOS_PER_SCHEDULER_TICK = NANOS_PER_SECOND / SCHEDULER_CLOCK_HZ;

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_TIME_H_
