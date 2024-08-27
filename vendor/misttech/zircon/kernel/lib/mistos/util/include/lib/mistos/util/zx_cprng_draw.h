// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ZX_CPRNG_DRAW_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ZX_CPRNG_DRAW_H_

#include <zircon/types.h>

#ifdef __cplusplus
extern "C" {
#endif

void zx_cprng_draw(void* buffer, size_t len);

#ifdef __cplusplus
}
#endif

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ZX_CPRNG_DRAW_H_
