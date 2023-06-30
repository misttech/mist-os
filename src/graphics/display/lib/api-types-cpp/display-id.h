// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DISPLAY_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DISPLAY_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display/DisplayId`.
DEFINE_STRONG_INT(DisplayId, uint64_t);

constexpr inline DisplayId ToDisplayId(uint64_t banjo_display_id) {
  return DisplayId(banjo_display_id);
}
constexpr inline DisplayId ToDisplayId(fuchsia_hardware_display::wire::DisplayId fidl_display_id) {
  return DisplayId(fidl_display_id.value);
}
constexpr inline uint64_t ToBanjoDisplayId(DisplayId display_id) { return display_id.value(); }
constexpr inline fuchsia_hardware_display::wire::DisplayId ToFidlDisplayId(DisplayId display_id) {
  return {.value = display_id.value()};
}

constexpr DisplayId kInvalidDisplayId(fuchsia_hardware_display::wire::kInvalidDispId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DISPLAY_ID_H_
