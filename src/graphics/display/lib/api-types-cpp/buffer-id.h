// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <zircon/assert.h>

#include <cstdint>
#include <limits>

#include "src/graphics/display/lib/api-types-cpp/buffer-collection-id.h"

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display/BufferId`].
//
// See `::fuchsia_hardware_display::wire::BufferId` for references.
struct BufferId {
  BufferCollectionId buffer_collection_id;
  uint32_t buffer_index;
};

constexpr inline BufferId ToBufferId(
    const fuchsia_hardware_display::wire::BufferId fidl_buffer_id) {
  ZX_DEBUG_ASSERT(fidl_buffer_id.buffer_index >= 0);
  ZX_DEBUG_ASSERT(fidl_buffer_id.buffer_index <= std::numeric_limits<uint32_t>::max());
  return BufferId{
      .buffer_collection_id = ToBufferCollectionId(fidl_buffer_id.buffer_collection_id),
      .buffer_index = fidl_buffer_id.buffer_index,
  };
}
constexpr inline fuchsia_hardware_display::wire::BufferId ToFidlBufferId(BufferId buffer_id) {
  ZX_DEBUG_ASSERT(buffer_id.buffer_index >= 0);
  ZX_DEBUG_ASSERT(buffer_id.buffer_index <= std::numeric_limits<uint32_t>::max());
  return {
      .buffer_collection_id = ToFidlBufferCollectionId(buffer_id.buffer_collection_id),
      .buffer_index = buffer_id.buffer_index,
  };
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_ID_H_
