// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_ID_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display.types/ModeId`.
DEFINE_STRONG_INT(ModeId, uint16_t);

constexpr ModeId ToModeId(uint16_t banjo_mode_id) { return ModeId(banjo_mode_id); }

constexpr ModeId ToModeId(fuchsia_hardware_display_types::wire::ModeId fidl_mode_id) {
  return ModeId(fidl_mode_id.value);
}

constexpr uint16_t ToBanjoModeId(ModeId mode_id) { return mode_id.value(); }

constexpr fuchsia_hardware_display_types::wire::ModeId ToFidlModeId(ModeId mode_id) {
  return {.value = mode_id.value()};
}

constexpr ModeId kInvalidModeId(fuchsia_hardware_display_types::wire::kInvalidModeId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_ID_H_
