// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_IMAGE_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_IMAGE_ID_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display.engine/ImageId`.
//
// The Banjo API between the Display Coordinator and drivers currently refers to
// this concept as "image handle". This name will be phased out when the
// API is migrated from Banjo to FIDL.
//
// See `ImageId` for the type used at the interface between the display
// coordinator and clients such as Scenic.
//
// TODO(https://fxbug.dev/42079544): Unify this type with `DriverCaptureImageId`
// when unifying image ID namespaces.
DEFINE_STRONG_INT(DriverImageId, uint64_t);

constexpr DriverImageId ToDriverImageId(uint64_t banjo_driver_image_id) {
  return DriverImageId(banjo_driver_image_id);
}

constexpr DriverImageId ToDriverImageId(
    fuchsia_hardware_display_engine::wire::ImageId fidl_driver_image_id) {
  return DriverImageId(fidl_driver_image_id.value);
}

constexpr uint64_t ToBanjoDriverImageId(DriverImageId driver_image_id) {
  return driver_image_id.value();
}

constexpr fuchsia_hardware_display_engine::wire::ImageId ToFidlDriverImageId(
    DriverImageId driver_image_id) {
  return fuchsia_hardware_display_engine::wire::ImageId{.value = driver_image_id.value()};
}

constexpr DriverImageId kInvalidDriverImageId(INVALID_ID);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_IMAGE_ID_H_
