// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CAPTURE_IMAGE_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CAPTURE_IMAGE_ID_H_

#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// Type-safe wrapper around capture image IDs exposed to display drivers.
//
// The Banjo API between the Display Coordinator and drivers currently refers to
// this concept as "capture image handle". This name will be phased out when the
// API is migrated from Banjo to FIDL.
//
// TODO(fxbug.dev/129092): Remove this type when unifying image ID namespaces.
DEFINE_STRONG_INT(DriverCaptureImageId, uint64_t);

constexpr inline DriverCaptureImageId ToDriverCaptureImageId(
    uint64_t banjo_driver_capture_image_id) {
  return DriverCaptureImageId(banjo_driver_capture_image_id);
}
constexpr inline uint64_t ToBanjoDriverCaptureImageId(
    DriverCaptureImageId driver_capture_image_id) {
  return driver_capture_image_id.value();
}

constexpr DriverCaptureImageId kInvalidDriverCaptureImageId(INVALID_ID);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CAPTURE_IMAGE_ID_H_
