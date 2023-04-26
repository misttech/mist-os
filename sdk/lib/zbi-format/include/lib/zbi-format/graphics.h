// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_GRAPHICS_H_
#define LIB_ZBI_FORMAT_GRAPHICS_H_

#include <stdint.h>

// ZBI_TYPE_FRAMEBUFFER payload.
typedef struct {
  // Physical memory address.
  uint64_t base;

  // Pixel layout and format.
  // See [../pixelformat.h](<zircon/pixelformat.h>).
  uint32_t width;
  uint32_t height;
  uint32_t stride;
  uint32_t format;
} zbi_swfb_t;

#endif  // LIB_ZBI_FORMAT_GRAPHICS_H_
