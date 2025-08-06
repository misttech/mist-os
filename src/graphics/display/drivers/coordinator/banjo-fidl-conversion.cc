// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/banjo-fidl-conversion.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/fdf/cpp/arena.h>

#include "src/graphics/display/lib/api-types/cpp/color-conversion.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"

namespace display_coordinator {

fuchsia_hardware_display_engine::wire::DisplayConfig ToFidlDisplayConfig(
    const display_config_t& banjo_config, fdf::Arena& arena) {
  fidl::VectorView<fuchsia_hardware_display_engine::wire::Layer> layers(arena,
                                                                        banjo_config.layers_count);
  for (size_t i = 0; i < banjo_config.layers_count; ++i) {
    layers[i] = display::DriverLayer(banjo_config.layers_list[i]).ToFidl();
  }

  const fuchsia_hardware_display_engine::wire::DisplayTiming timing =
      display::ToFidlDisplayTiming(display::ToDisplayTiming(banjo_config.timing));

  return fuchsia_hardware_display_engine::wire::DisplayConfig{
      .display_id = display::DisplayId(banjo_config.display_id).ToFidl(),
      .mode_id = display::ModeId(banjo_config.mode_id).ToFidl(),
      .timing = timing,
      .color_conversion = display::ColorConversion(banjo_config.color_conversion).ToFidl(),
      .layers = layers,
  };
}

}  // namespace display_coordinator
