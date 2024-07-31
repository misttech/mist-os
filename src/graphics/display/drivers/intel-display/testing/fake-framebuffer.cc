// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-display/testing/fake-framebuffer.h"

#include <zircon/compiler.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <cstdint>
#include <mutex>

namespace fake_framebuffer {

__CONSTINIT std::mutex g_lock = {};
__CONSTINIT Framebuffer g_framebuffer = {};

void SetFramebuffer(const Framebuffer& buffer) {
  std::lock_guard guard(g_lock);
  g_framebuffer = buffer;
}

}  // namespace fake_framebuffer

__BEGIN_CDECLS

__EXPORT
zx_status_t zx_framebuffer_get_info(zx_handle_t resource, uint32_t* format, uint32_t* width,
                                    uint32_t* height, uint32_t* stride) {
  std::lock_guard guard(fake_framebuffer::g_lock);
  const fake_framebuffer::Framebuffer& framebuffer = fake_framebuffer::g_framebuffer;
  if (framebuffer.status == ZX_OK) {
    *format = framebuffer.format;
    *width = framebuffer.width;
    *height = framebuffer.height;
    *stride = framebuffer.stride;
  }
  return framebuffer.status;
}

__END_CDECLS
