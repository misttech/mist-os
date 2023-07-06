// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_EVENT_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_EVENT_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display/EventId`.
DEFINE_STRONG_INT(EventId, uint64_t);

constexpr inline EventId ToEventId(fuchsia_hardware_display::wire::EventId fidl_event_id) {
  return EventId(fidl_event_id.value);
}
constexpr inline fuchsia_hardware_display::wire::EventId ToFidlEventId(EventId event_id) {
  return {.value = event_id.value()};
}

constexpr EventId kInvalidEventId(fuchsia_hardware_display::wire::kInvalidDispId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_EVENT_ID_H_
