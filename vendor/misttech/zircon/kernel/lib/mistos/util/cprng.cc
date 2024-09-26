// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/cprng.h"

#include <lib/crypto/global_prng.h>
#include <zircon/types.h>

#include <explicit-memory/bytes.h>

constexpr size_t kMaxCPRNGDraw = ZX_CPRNG_DRAW_MAX_LEN;

void cprng_draw(void* buffer, size_t len) {
  auto ptr = reinterpret_cast<uint8_t*>(buffer);
  while (len != 0) {
    size_t chunk = len;
    if (chunk > ZX_CPRNG_DRAW_MAX_LEN)
      chunk = ZX_CPRNG_DRAW_MAX_LEN;
    zx_status_t status = cprng_draw_once(reinterpret_cast<void*>(ptr), chunk);
    //  zx_cprng_draw_once shouldn't fail unless given bogus arguments.
    if (unlikely(status != ZX_OK)) {
      // We loop around __builtin_trap in case __builtin_trap doesn't
      // actually terminate the process.
      while (true) {
        ZX_PANIC("Failed to cprng_draw_once, error %d", status);
      }
    }
    ptr += chunk;
    len -= chunk;
  }
}

zx_status_t cprng_draw_once(void* buffer, size_t len) {
  if (len > kMaxCPRNGDraw)
    return ZX_ERR_INVALID_ARGS;

  uint8_t kernel_buf[kMaxCPRNGDraw];
  // Ensure we get rid of the stack copy of the random data as this function returns.
  explicit_memory::ZeroDtor<uint8_t> zero_guard(kernel_buf, sizeof(kernel_buf));

  auto prng = crypto::global_prng::GetInstance();
  ASSERT(prng->is_thread_safe());
  prng->Draw(kernel_buf, len);

  memcpy(buffer, kernel_buf, len);

  return ZX_OK;
}

ktl::span<uint8_t> cprng_draw_uninit(ktl::span<uint8_t> buffer) {
  cprng_draw(buffer.data(), buffer.size());
  return buffer;
}
