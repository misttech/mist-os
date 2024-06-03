// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/pixel.h"

#include <lib/syslog/cpp/macros.h>

namespace utils {

namespace {
// List of supported pixel formats
std::vector<fuchsia::images2::PixelFormat> kSupportedPixelFormats = {
    fuchsia::images2::PixelFormat::B8G8R8A8, fuchsia::images2::PixelFormat::R8G8B8A8};
}  // namespace

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

Pixel Pixel::FromVmo(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y,
                     fuchsia::images2::PixelFormat type) {
  if (type == fuchsia::images2::PixelFormat::B8G8R8A8) {
    return FromVmoBgra(vmo_host, stride, x, y);
  }
  FX_DCHECK(type == fuchsia::images2::PixelFormat::R8G8B8A8);
  return FromVmoRgba(vmo_host, stride, x, y);
}

Pixel Pixel::FromVmo(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y,
                     fuchsia::sysmem::PixelFormatType type) {
  if (type == fuchsia::sysmem::PixelFormatType::BGRA32) {
    return FromVmoBgra(vmo_host, stride, x, y);
  }
  FX_DCHECK(type == fuchsia::sysmem::PixelFormatType::R8G8B8A8);
  return FromVmoRgba(vmo_host, stride, x, y);
}

Pixel Pixel::FromVmoRgba(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y) {
  uint8_t r = vmo_host[y * stride * 4 + x * 4];
  uint8_t g = vmo_host[y * stride * 4 + x * 4 + 1];
  uint8_t b = vmo_host[y * stride * 4 + x * 4 + 2];
  uint8_t a = vmo_host[y * stride * 4 + x * 4 + 3];
  return utils::Pixel(b, g, r, a);
}

Pixel Pixel::FromVmoBgra(const uint8_t* vmo_host, uint32_t stride, uint32_t x, uint32_t y) {
  uint8_t b = vmo_host[y * stride * 4 + x * 4];
  uint8_t g = vmo_host[y * stride * 4 + x * 4 + 1];
  uint8_t r = vmo_host[y * stride * 4 + x * 4 + 2];
  uint8_t a = vmo_host[y * stride * 4 + x * 4 + 3];
  return utils::Pixel(b, g, r, a);
}

std::vector<uint8_t> Pixel::ToFormat(fuchsia::images2::PixelFormat type) {
  if (type == fuchsia::images2::PixelFormat::B8G8R8A8) {
    return ToBgra();
  }
  FX_DCHECK(type == fuchsia::images2::PixelFormat::R8G8B8A8);
  return ToRgba();
}

std::vector<uint8_t> Pixel::ToFormat(fuchsia::sysmem::PixelFormatType type) {
  if (type == fuchsia::sysmem::PixelFormatType::BGRA32) {
    return ToBgra();
  }
  FX_DCHECK(type == fuchsia::sysmem::PixelFormatType::R8G8B8A8);
  return ToRgba();
}

bool Pixel::IsFormatSupported(fuchsia::images2::PixelFormat type) {
  return std::any_of(kSupportedPixelFormats.begin(), kSupportedPixelFormats.end(),
                     [type](fuchsia::images2::PixelFormat supported) { return supported == type; });
}
bool Pixel::IsFormatSupported(fuchsia::sysmem::PixelFormatType type) {
  fuchsia::images2::PixelFormat v2_pixel_format =
      static_cast<fuchsia::images2::PixelFormat>(static_cast<uint32_t>(type));
  return IsFormatSupported(v2_pixel_format);
}

std::ostream& operator<<(std::ostream& stream, const Pixel& pixel) {
  return stream << "{Pixel:"
                << " r:" << static_cast<unsigned int>(pixel.red)
                << " g:" << static_cast<unsigned int>(pixel.green)
                << " b:" << static_cast<unsigned int>(pixel.blue)
                << " a:" << static_cast<unsigned int>(pixel.alpha) << "}";
}

}  // namespace utils
