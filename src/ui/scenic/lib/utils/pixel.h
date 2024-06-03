// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_UI_SCENIC_LIB_UTILS_PIXEL_H_
#define SRC_UI_SCENIC_LIB_UTILS_PIXEL_H_

#include <fuchsia/sysmem/cpp/fidl.h>
#include <math.h>

#include <cstdint>
#include <ostream>

namespace utils {

// Represents a Pixel using the sRGB color space.
struct Pixel {
  uint8_t blue = 0;
  uint8_t green = 0;
  uint8_t red = 0;
  uint8_t alpha = 0;

  Pixel(uint8_t blue, uint8_t green, uint8_t red, uint8_t alpha)
      : blue(blue), green(green), red(red), alpha(alpha) {}

  static Pixel FromUnormBgra(float blue, float green, float red, float alpha);

  bool operator==(const Pixel& rhs) const {
    return blue == rhs.blue && green == rhs.green && red == rhs.red && alpha == rhs.alpha;
  }

  static Pixel FromVmo(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y,
                       fuchsia::images2::PixelFormat type);
  static Pixel FromVmo(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y,
                       fuchsia::sysmem::PixelFormatType type);

  static Pixel FromVmoBgra(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y);

  static Pixel FromVmoRgba(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y);

  std::vector<uint8_t> ToFormat(fuchsia::images2::PixelFormat type);

  std::vector<uint8_t> ToFormat(fuchsia::sysmem::PixelFormatType type);

  std::vector<uint8_t> ToBgra() { return {blue, green, red, alpha}; }

  std::vector<uint8_t> ToRgba() { return {red, green, blue, alpha}; }

  static bool IsFormatSupported(fuchsia::images2::PixelFormat type);
  // deprecated; use other overload just above
  static bool IsFormatSupported(fuchsia::sysmem::PixelFormatType type);

  inline bool operator!=(const Pixel& rhs) const { return !(*this == rhs); }

  bool operator<(const Pixel& other) const {
    return std::tie(blue, green, red, alpha) <
           std::tie(other.blue, other.green, other.red, other.alpha);
  }
};

std::ostream& operator<<(std::ostream& stream, const utils::Pixel& pixel);

inline static const Pixel kBlack = Pixel(0, 0, 0, 255);
inline static const Pixel kBlue = Pixel(255, 0, 0, 255);
inline static const Pixel kRed = Pixel(0, 0, 255, 255);
inline static const Pixel kMagenta = Pixel(255, 0, 255, 255);
inline static const Pixel kGreen = Pixel(0, 255, 0, 255);

}  // namespace utils

#endif  // SRC_UI_SCENIC_LIB_UTILS_PIXEL_H_
