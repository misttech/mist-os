// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_CPRNG_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_CPRNG_H_

#include <zircon/types.h>

#include <ktl/span.h>

void cprng_draw(void* buffer, size_t len);
zx_status_t cprng_draw_once(void* buffer, size_t len);
ktl::span<uint8_t> cprng_draw_uninit(ktl::span<uint8_t> buffer);

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_CPRNG_H_
