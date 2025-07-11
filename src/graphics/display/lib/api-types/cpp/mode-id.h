// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_ID_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>
#include <type_traits>

#include "src/graphics/display/lib/api-types/cpp/id-type.h"

namespace display::internal {

using ModeIdTraits =
    DefaultIdTypeTraits<uint16_t, fuchsia_hardware_display_types::wire::ModeId, uint16_t>;

}  // namespace display::internal

namespace display {

// More useful representation of `fuchsia.hardware.display.types/ModeId`.
using ModeId = display::internal::IdType<display::internal::ModeIdTraits>;

constexpr ModeId ToModeId(uint16_t banjo_mode_id) { return ModeId(banjo_mode_id); }

constexpr ModeId ToModeId(fuchsia_hardware_display_types::wire::ModeId fidl_mode_id) {
  return ModeId(fidl_mode_id);
}

constexpr uint16_t ToBanjoModeId(ModeId mode_id) { return mode_id.ToBanjo(); }

constexpr fuchsia_hardware_display_types::wire::ModeId ToFidlModeId(ModeId mode_id) {
  return mode_id.ToFidl();
}

constexpr ModeId kInvalidModeId(fuchsia_hardware_display_types::wire::kInvalidModeId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_ID_H_
