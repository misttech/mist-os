// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CAPTURE_IMAGE_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CAPTURE_IMAGE_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// Type-safe wrapper around capture image IDs exposed to Coordinator clients.
//
// TODO(fxbug.dev/129092): Remove this type when unifying image ID namespaces.
DEFINE_STRONG_INT(CaptureImageId, uint64_t);

constexpr inline CaptureImageId ToCaptureImageId(uint64_t fidl_capture_image_id) {
  return CaptureImageId(fidl_capture_image_id);
}
constexpr inline uint64_t ToFidlCaptureImageId(CaptureImageId capture_image_id) {
  return capture_image_id.value();
}

constexpr CaptureImageId kInvalidCaptureImageId(fuchsia_hardware_display::wire::kInvalidDispId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CAPTURE_IMAGE_ID_H_
