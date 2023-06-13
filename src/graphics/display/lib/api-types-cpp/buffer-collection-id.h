// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_COLLECTION_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_COLLECTION_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire_types.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `collection_id` field in FIDL protocol
// `fuchsia.hardware.display/Controller` methods importing, using and releasing
// sysmem BufferCollections.
//
// Not to be confused with `DriverBufferCollectionId` which represents
// BufferCollections imported to display drivers.
DEFINE_STRONG_INT(BufferCollectionId, uint64_t);

constexpr inline BufferCollectionId ToBufferCollectionId(uint64_t fidl_buffer_collection_id) {
  return BufferCollectionId(fidl_buffer_collection_id);
}
constexpr inline uint64_t ToFidlBufferCollectionId(BufferCollectionId buffer_collection_id) {
  return buffer_collection_id.value();
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_COLLECTION_ID_H_
