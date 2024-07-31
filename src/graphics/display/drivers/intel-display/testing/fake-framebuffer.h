// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_TESTING_FAKE_FRAMEBUFFER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_TESTING_FAKE_FRAMEBUFFER_H_

#include <zircon/compiler.h>
#include <zircon/types.h>

#include <mutex>

namespace fake_framebuffer {

// This library provides a fake replacement `zx_framebuffer_get_info()`
// implementation for testing driver code in an unprivileged environment.
// It works by defining strong symbols for the `zx_framebuffer_get_info()`
// syscall.

// Data source for the fake `zx_framebuffer_get_info` implementation.
struct Framebuffer {
  zx_status_t status = ZX_OK;
  uint32_t format = 0u;
  uint32_t width = 0u;
  uint32_t height = 0u;
  uint32_t stride = 0u;
};

extern std::mutex g_lock;
extern Framebuffer g_framebuffer __TA_GUARDED(g_lock);

void SetFramebuffer(const Framebuffer& buffer);

}  // namespace fake_framebuffer

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_TESTING_FAKE_FRAMEBUFFER_H_
