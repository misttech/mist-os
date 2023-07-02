// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display/ImageIdValue`.
DEFINE_STRONG_INT(ImageId, uint64_t);

constexpr inline ImageId ToImageId(uint64_t fidl_image_id_value) {
  return ImageId(fidl_image_id_value);
}
constexpr inline uint64_t ToFidlImageIdValue(ImageId image_id) { return image_id.value(); }

constexpr ImageId kInvalidImageId(fuchsia_hardware_display::wire::kInvalidDispId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_ID_H_
