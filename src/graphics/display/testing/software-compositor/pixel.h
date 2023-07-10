// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_SOFTWARE_COMPOSITOR_PIXEL_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_SOFTWARE_COMPOSITOR_PIXEL_H_

#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

#include <cstdint>

namespace software_compositor {

// Specifies the channel layout and internal channel representation within a
// pixel.
//
// Corresponds to `VkFormat` in Vulkan specification.
enum class PixelFormat {
  kRgba8888,
  kBgra8888,
};

constexpr int GetBytesPerPixel(PixelFormat pixel_format) {
  switch (pixel_format) {
    case PixelFormat::kRgba8888:
    case PixelFormat::kBgra8888:
      return 4;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid pixel format %d", static_cast<int>(pixel_format));
  }
}

// A Pixel (or texels for textures) represents a color on a specific point
// on the dot-matrix display. It may contain one or multiple channels (color
// components).
//
// PixelData stores the raw byte representation of channel values of a pixel
// (i.e. pixel data) without caring about the internal channel representation
// or the pixel format.
struct PixelData {
  std::array<uint8_t, 4> data;

  static PixelData FromRaw(cpp20::span<const uint8_t> raw);
  PixelData Convert(PixelFormat from, PixelFormat to) const;
};

}  // namespace software_compositor

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_SOFTWARE_COMPOSITOR_PIXEL_H_
