// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_PIXEL_FORMAT_PIXEL_FORMAT_H_
#define SRC_GRAPHICS_DISPLAY_LIB_PIXEL_FORMAT_PIXEL_FORMAT_H_

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <lib/stdcompat/span.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>

#include <cstdint>

#include <fbl/vector.h>

namespace display {

// Indicates a pixel format type being migrated from `zx_pixel_format_t`
// to `fuchsia.images2.PixelFormat`. A pixel format type being migrated
// may contain values of either type.
//
// Note that the way `zx_pixel_format_t` values are defined makes the two types
// easily separable: It's deterministic to know the underlying type of an
// `AnyPixelFormat` value; any implicit conversion from `zx_pixel_format_t` or
// `fuchsia.images2.PixelFormat` enum to `AnyPixelFormat` will be safe.
using AnyPixelFormat = uint32_t;

// The underlying format of fuchsia.images2.PixelFormat enum value.
using FuchsiaImages2PixelFormatEnumValue = uint32_t;

// Converts a value of AnyPixelFormat to its corresponding fuchsia.images2.
// PixelFormat value.
//
// - Returns ZX_ERR_INVALID_ARGS if the value is not a valid format of its
//   "expected" type.
// - Otherwise, returns ZX_ERR_NOT_SUPPORTED if the format cannot be represented
//   in fuchsia.images2.PixelFormat.
// - Otherwise, returns the converted pixel format.
zx::result<fuchsia_images2::wire::PixelFormat> AnyPixelFormatToImages2PixelFormat(
    AnyPixelFormat any_pixel_format);

// Converts values of AnyPixelFormat to corresponding fuchsia.images2.
// PixelFormat values. The converted vector will be de-duplicated.
//
// - Returns the error value of AnyPixelFormatToImages2PixelFormat() if any of
//   the pixel format cannot be converted.
// - Otherwise, returns the converted pixel format vector.
zx::result<std::vector<fuchsia_images2::wire::PixelFormat>>
AnyPixelFormatToImages2PixelFormatStdVector(cpp20::span<const AnyPixelFormat> any_pixel_formats);

// Converts the values of AnyPixelFormat to corresponding fuchsia.images2.
// PixelFormat values. The converted vector will be de-duplicated.
//
// - Returns ZX_ERR_NO_MEMORY if the vector memory cannot be allocated.
// - Returns the error value of AnyPixelFormatToImages2PixelFormat() if any of
//   the pixel format cannot be converted.
// - Otherwise, returns the converted pixel format vector.
zx::result<fbl::Vector<fuchsia_images2::wire::PixelFormat>>
AnyPixelFormatToImages2PixelFormatFblVector(cpp20::span<const AnyPixelFormat> any_pixel_formats);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_PIXEL_FORMAT_PIXEL_FORMAT_H_
