// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/pixel-format/pixel-format.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <lib/image-format/image_format.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/pixelformat.h>

#include <fbl/alloc_checker.h>

namespace display {

zx::result<fuchsia_images2::wire::PixelFormat> AnyPixelFormatToImages2PixelFormat(
    AnyPixelFormat any_pixel_format) {
  // As a heuristic, we treat values >= 0x10000 as a `zx_pixel_format_t`.
  constexpr AnyPixelFormat kZxPixelFormatThreshold = 0x10000;
  if (any_pixel_format >= kZxPixelFormatThreshold) {
    switch (any_pixel_format) {
      case ZX_PIXEL_FORMAT_RGB_565:
      case ZX_PIXEL_FORMAT_RGB_332:
      case ZX_PIXEL_FORMAT_RGB_2220:
      case ZX_PIXEL_FORMAT_ARGB_8888:
      case ZX_PIXEL_FORMAT_RGB_x888:
      case ZX_PIXEL_FORMAT_MONO_8:
      case ZX_PIXEL_FORMAT_I420:
      case ZX_PIXEL_FORMAT_NV12:
      case ZX_PIXEL_FORMAT_RGB_888:
      case ZX_PIXEL_FORMAT_ABGR_8888:
      case ZX_PIXEL_FORMAT_BGR_888x:
      case ZX_PIXEL_FORMAT_ARGB_2_10_10_10:
      case ZX_PIXEL_FORMAT_ABGR_2_10_10_10: {
        fpromise::result convert_result =
            ImageFormatConvertZxToSysmemPixelFormat_v2(any_pixel_format);
        if (!convert_result.is_ok()) {
          FX_LOGS(ERROR) << "Failed to convert zx_pixel_format_t " << any_pixel_format;
          return zx::error(ZX_ERR_NOT_SUPPORTED);
        }
        return zx::ok(convert_result.take_value());
      }
    }
  }
  // Otherwise, we assume the value to be a `fuchsia.images2.PixelFormat` enum.
  auto images2_format = static_cast<fuchsia_images2::wire::PixelFormat>(any_pixel_format);
  if (images2_format.IsUnknown()) {
    FX_LOGS(ERROR) << "Unknown display pixel format " << any_pixel_format;
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  return zx::ok(images2_format);
}

zx::result<std::vector<fuchsia_images2::wire::PixelFormat>>
AnyPixelFormatToImages2PixelFormatStdVector(cpp20::span<const AnyPixelFormat> any_pixel_formats) {
  std::vector<fuchsia_images2::wire::PixelFormat> result;
  for (const AnyPixelFormat any_pixel_format : any_pixel_formats) {
    zx::result sysmem2_format(AnyPixelFormatToImages2PixelFormat(any_pixel_format));
    if (sysmem2_format.is_ok()) {
      if (std::find(result.begin(), result.end(), sysmem2_format.value()) == result.end()) {
        result.push_back(sysmem2_format.value());
      }
    } else {
      return zx::error(sysmem2_format.error_value());
    }
  }
  return zx::ok(std::move(result));
}

zx::result<fbl::Vector<fuchsia_images2::wire::PixelFormat>>
AnyPixelFormatToImages2PixelFormatFblVector(cpp20::span<const AnyPixelFormat> any_pixel_formats) {
  fbl::AllocChecker ac;
  fbl::Vector<fuchsia_images2::wire::PixelFormat> result;
  result.reserve(any_pixel_formats.size(), &ac);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  for (const AnyPixelFormat any_pixel_format : any_pixel_formats) {
    zx::result sysmem2_format(AnyPixelFormatToImages2PixelFormat(any_pixel_format));
    if (sysmem2_format.is_ok()) {
      if (std::find(result.begin(), result.end(), sysmem2_format.value()) == result.end()) {
        result.push_back(sysmem2_format.value());
      }
    } else {
      return zx::error(sysmem2_format.error_value());
    }
  }
  return zx::ok(std::move(result));
}

}  // namespace display
