// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/pixel.h"

namespace utils {

uint8_t LinearToSrgb(const float val) {
  // Function to convert from linear RGB to sRGB.
  // (https://en.wikipedia.org/wiki/SRGB#From_CIE_XYZ_to_sRGB)
  if (0.f <= val && val <= 0.0031308f) {
    return static_cast<uint8_t>(roundf((val * 12.92f) * 255U));
  }
  return static_cast<uint8_t>(roundf(((powf(val, 1.0f / 2.4f) * 1.055f) - 0.055f) * 255U));
}

Pixel Pixel::FromUnormBgra(float blue, float green, float red, float alpha) {
  return Pixel{LinearToSrgb(blue), LinearToSrgb(green), LinearToSrgb(red),
               static_cast<uint8_t>(roundf(alpha * 255U))};
}

std::ostream& operator<<(std::ostream& stream, const Pixel& pixel) {
  return stream << "{Pixel:" << " r:" << static_cast<unsigned int>(pixel.red)
                << " g:" << static_cast<unsigned int>(pixel.green)
                << " b:" << static_cast<unsigned int>(pixel.blue)
                << " a:" << static_cast<unsigned int>(pixel.alpha) << "}";
}

}  // namespace utils
