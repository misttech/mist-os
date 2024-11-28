// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_BUFFER_USAGE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_BUFFER_USAGE_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/ImageBufferUsage`].
struct ImageBufferUsage {
  ImageTilingType tiling_type;
};

constexpr bool operator==(const ImageBufferUsage& lhs, const ImageBufferUsage& rhs) {
  return lhs.tiling_type == rhs.tiling_type;
}

constexpr bool operator!=(const ImageBufferUsage& lhs, const ImageBufferUsage& rhs) {
  return !(lhs == rhs);
}

constexpr ImageBufferUsage ToImageBufferUsage(
    fuchsia_hardware_display_types::wire::ImageBufferUsage fidl_image_buffer_usage) {
  return ImageBufferUsage{
      .tiling_type = ImageTilingType(fidl_image_buffer_usage.tiling_type),
  };
}

constexpr ImageBufferUsage ToImageBufferUsage(image_buffer_usage_t banjo_image_buffer_usage) {
  return ImageBufferUsage{
      .tiling_type = ImageTilingType(banjo_image_buffer_usage.tiling_type),
  };
}

constexpr fuchsia_hardware_display_types::wire::ImageBufferUsage ToFidlImageBufferUsage(
    const ImageBufferUsage& image_buffer_usage) {
  return fuchsia_hardware_display_types::wire::ImageBufferUsage{
      .tiling_type = image_buffer_usage.tiling_type.ToFidl(),
  };
}

constexpr image_buffer_usage_t ToBanjoImageBufferUsage(const ImageBufferUsage& image_buffer_usage) {
  return image_buffer_usage_t{
      .tiling_type = image_buffer_usage.tiling_type.ToBanjo(),
  };
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_BUFFER_USAGE_H_
