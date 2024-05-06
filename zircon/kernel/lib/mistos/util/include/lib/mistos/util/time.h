// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TIME_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TIME_H_

#include <zircon/types.h>

#ifdef __cplusplus
extern "C" {
#endif

zx_time_t zx_deadline_after(zx_duration_t nanoseconds);
zx_time_t zx_clock_get_monotonic(void);
zx_ticks_t zx_ticks_per_second(void);

#ifdef __cplusplus
}
#endif

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TIME_H_
