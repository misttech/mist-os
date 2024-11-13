// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_ID_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <zircon/assert.h>

#include <cstdint>
#include <limits>

#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.engine/BufferId`].
//
// See `::fuchsia_hardware_display_engine::wire::BufferId` for references.
//
// See `BufferId` for the type used at the interface between the display
// coordinator and clients such as Scenic.
struct DriverBufferId {
  DriverBufferCollectionId buffer_collection_id;
  uint32_t buffer_index;
};

constexpr DriverBufferId ToDriverBufferId(
    const fuchsia_hardware_display_engine::wire::BufferId fidl_buffer_id) {
  ZX_DEBUG_ASSERT(fidl_buffer_id.buffer_index >= 0);
  ZX_DEBUG_ASSERT(fidl_buffer_id.buffer_index <= std::numeric_limits<uint32_t>::max());
  return DriverBufferId{
      .buffer_collection_id = ToDriverBufferCollectionId(fidl_buffer_id.buffer_collection_id),
      .buffer_index = fidl_buffer_id.buffer_index,
  };
}

constexpr fuchsia_hardware_display_engine::wire::BufferId ToFidlDriverBufferId(
    DriverBufferId driver_buffer_id) {
  ZX_DEBUG_ASSERT(driver_buffer_id.buffer_index >= 0);
  ZX_DEBUG_ASSERT(driver_buffer_id.buffer_index <= std::numeric_limits<uint32_t>::max());
  return {
      .buffer_collection_id = ToFidlDriverBufferCollectionId(driver_buffer_id.buffer_collection_id),
      .buffer_index = driver_buffer_id.buffer_index,
  };
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_BUFFER_ID_H_
