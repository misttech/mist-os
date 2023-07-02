// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_COLLECTION_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_COLLECTION_ID_H_

#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `collection_id` field in banjo protocol
// `fuchsia.hardware.display.controller/DisplayControllerImpl` methods
// importing, using and releasing sysmem BufferCollections.
//
// Not to be confused with `BufferCollectionId` which represents
// BufferCollections imported by display applications to the coordinator
// via display Controller FIDL API.
DEFINE_STRONG_INT(DriverBufferCollectionId, uint64_t);

constexpr inline DriverBufferCollectionId ToDriverBufferCollectionId(
    uint64_t banjo_driver_buffer_collection_id) {
  return DriverBufferCollectionId(banjo_driver_buffer_collection_id);
}
constexpr inline uint64_t ToBanjoDriverBufferCollectionId(
    DriverBufferCollectionId driver_buffer_collection_id) {
  return driver_buffer_collection_id.value();
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_COLLECTION_ID_H_
