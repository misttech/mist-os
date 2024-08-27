// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_CPRNG_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_CPRNG_H_

#include <lib/fit/result.h>
#include <zircon/types.h>

#include <ktl/byte.h>
#include <ktl/span.h>

void cprng_draw(uint8_t* buffer, size_t size);
void cprng_draw_uninit(ktl::span<uint8_t> buffer);
fit::result<zx_status_t> cprng_add_entropy(const ktl::span<uint8_t> buffer);

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_CPRNG_H_
