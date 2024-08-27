// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/cprng.h"

#include <lib/fit/result.h>
#include <lib/mistos/util/zx_cprng_draw.h>
#include <zircon/syscalls.h>

#include <ktl/span.h>

void cprng_draw(uint8_t* buffer, size_t size) {
  ktl::span<uint8_t> buff{buffer, size};
  cprng_draw_uninit(buff);
}

void cprng_draw_uninit(ktl::span<uint8_t> buffer) { zx_cprng_draw(buffer.data(), buffer.size()); }

fit::result<zx_status_t> cprng_add_entropy(const ktl::span<uint8_t> buffer) {
  if (zx_status_t status = zx_cprng_add_entropy(buffer.data(), buffer.size()); status != ZX_OK) {
    return fit::error(status);
  }
  return fit::ok();
}
