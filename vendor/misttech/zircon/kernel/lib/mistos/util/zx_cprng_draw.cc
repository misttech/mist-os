// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/zx_cprng_draw.h"

#include <lib/user_copy/user_ptr.h>

#include <ktl/span.h>

extern zx_status_t sys_cprng_draw_once(user_out_ptr<void> buffer, size_t len);

void _zx_cprng_draw(user_out_ptr<void> buffer, size_t len) {
  while (len != 0) {
    size_t chunk = len;
    if (chunk > ZX_CPRNG_DRAW_MAX_LEN)
      chunk = ZX_CPRNG_DRAW_MAX_LEN;
    zx_status_t status = sys_cprng_draw_once(buffer, chunk);
    //  zx_cprng_draw_once shouldn't fail unless given bogus arguments.
    if (unlikely(status != ZX_OK)) {
      // We loop around __builtin_trap in case __builtin_trap doesn't
      // actually terminate the process.
      while (true) {
        __builtin_trap();
      }
    }
    buffer = buffer.byte_offset(chunk);
    len -= chunk;
  }
}
