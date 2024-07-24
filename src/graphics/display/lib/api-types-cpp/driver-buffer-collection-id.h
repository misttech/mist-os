// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_COLLECTION_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_COLLECTION_ID_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of
// `fuchsia.hardware.display.engine/BufferCollectionId`.
//
// The Banjo API between the Display Coordinator and drivers currently
// represents this concept as a `collection_id` argument on
// `fuchsia.hardware.display.controller/DisplayEngine` methods
// that import, use and release sysmem BufferCollections.
//
// See `BufferCollectionId` for the type used at the interface between the
// display coordinator and clients such as Scenic.
DEFINE_STRONG_INT(DriverBufferCollectionId, uint64_t);

constexpr DriverBufferCollectionId ToDriverBufferCollectionId(
    uint64_t banjo_driver_buffer_collection_id) {
  return DriverBufferCollectionId(banjo_driver_buffer_collection_id);
}

constexpr DriverBufferCollectionId ToDriverBufferCollectionId(
    fuchsia_hardware_display_engine::wire::BufferCollectionId fidl_driver_buffer_collection_id) {
  return DriverBufferCollectionId(fidl_driver_buffer_collection_id.value);
}

constexpr uint64_t ToBanjoDriverBufferCollectionId(
    DriverBufferCollectionId driver_buffer_collection_id) {
  return driver_buffer_collection_id.value();
}

constexpr fuchsia_hardware_display_engine::wire::BufferCollectionId ToFidlDriverBufferCollectionId(
    DriverBufferCollectionId driver_buffer_collection_id) {
  return fuchsia_hardware_display_engine::wire::BufferCollectionId{
      .value = driver_buffer_collection_id.value()};
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_COLLECTION_ID_H_
