// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_GRAPHICS_H_
#define LIB_ZBI_FORMAT_GRAPHICS_H_

#include <stdint.h>

typedef uint32_t zbi_pixel_format_t;

#define ZBI_PIXEL_FORMAT_NONE ((zbi_pixel_format_t)0x00000000)
#define ZBI_PIXEL_FORMAT_RGB_565 ((zbi_pixel_format_t)0x00020001)
#define ZBI_PIXEL_FORMAT_RGB_332 ((zbi_pixel_format_t)0x00010002)
#define ZBI_PIXEL_FORMAT_RGB_2220 ((zbi_pixel_format_t)0x00010003)
#define ZBI_PIXEL_FORMAT_ARGB_8888 ((zbi_pixel_format_t)0x00040004)
#define ZBI_PIXEL_FORMAT_RGB_x888 ((zbi_pixel_format_t)0x00040005)
#define ZBI_PIXEL_FORMAT_MONO_8 ((zbi_pixel_format_t)0x00010007)
#define ZBI_PIXEL_FORMAT_GRAY_8 ((zbi_pixel_format_t)0x00010007)
#define ZBI_PIXEL_FORMAT_NV12 ((zbi_pixel_format_t)0x00010008)
#define ZBI_PIXEL_FORMAT_I420 ((zbi_pixel_format_t)0x00010009)
#define ZBI_PIXEL_FORMAT_RGB_888 ((zbi_pixel_format_t)0x00030009)
#define ZBI_PIXEL_FORMAT_ABGR_8888 ((zbi_pixel_format_t)0x0004000a)
#define ZBI_PIXEL_FORMAT_BGR_888x ((zbi_pixel_format_t)0x0004000b)
#define ZBI_PIXEL_FORMAT_ARGB_2_10_10_10 ((zbi_pixel_format_t)0x0004000c)
#define ZBI_PIXEL_FORMAT_ABGR_2_10_10_10 ((zbi_pixel_format_t)0x0004000d)
#define ZBI_PIXEL_FORMAT_BYTES(pf) (((pf) >> 16) & 7)

// ZBI_TYPE_FRAMEBUFFER payload.
typedef struct {
  // Physical memory address.
  uint64_t base;

  // Pixel layout and format.
  uint32_t width;
  uint32_t height;
  uint32_t stride;
  zbi_pixel_format_t format;
} zbi_swfb_t;

#endif  // LIB_ZBI_FORMAT_GRAPHICS_H_
