// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display/LayerId`.
//
// See `DriverLayerId` for the type used at the interface between the display
// coordinator and the display drivers.
DEFINE_STRONG_INT(LayerId, uint64_t);

constexpr inline LayerId ToLayerId(fuchsia_hardware_display::wire::LayerId fidl_layer_id) {
  return LayerId(fidl_layer_id.value);
}
constexpr inline fuchsia_hardware_display::wire::LayerId ToFidlLayerId(LayerId layer_id) {
  return {.value = layer_id.value()};
}
constexpr LayerId kInvalidLayerId(fuchsia_hardware_display::wire::kInvalidDispId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_ID_H_
