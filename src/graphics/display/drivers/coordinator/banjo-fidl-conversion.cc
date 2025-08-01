// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/banjo-fidl-conversion.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/fdf/cpp/arena.h>

#include <algorithm>
#include <span>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"

namespace display_coordinator {

namespace {

// `banjo_preoffsets` must point to an array of size 3.
fidl::Array<float, 3> ToFidlPreoffsets(const float* banjo_preoffsets) {
  fidl::Array<float, 3> fidl_preoffsets;
  std::ranges::copy(std::span(banjo_preoffsets, 3), fidl_preoffsets.begin());
  return fidl_preoffsets;
}

// `banjo_coefficients` must point to an array of size 3, and each element
// of `banjo_coefficients` must point to an array of size 3.
fidl::Array<fidl::Array<float, 3>, 3> ToFidlCoefficients(const float (*banjo_coefficients)[3]) {
  fidl::Array<fidl::Array<float, 3>, 3> fidl_coefficients;
  for (size_t i = 0; i < 3; ++i) {
    std::ranges::copy(std::span(banjo_coefficients[i], 3), fidl_coefficients[i].begin());
  }
  return fidl_coefficients;
}

// `banjo_postoffsets` must point to an array of size 3.
fidl::Array<float, 3> ToFidlPostoffsets(const float* banjo_postoffsets) {
  fidl::Array<float, 3> fidl_postoffsets;
  std::ranges::copy(std::span(banjo_postoffsets, 3), fidl_postoffsets.begin());
  return fidl_postoffsets;
}

}  // namespace

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
      .timing = timing,
      .color_conversion =
          {
              .preoffsets = ToFidlPreoffsets(banjo_config.color_conversion.preoffsets),
              .coefficients = ToFidlCoefficients(banjo_config.color_conversion.coefficients),
              .postoffsets = ToFidlPostoffsets(banjo_config.color_conversion.postoffsets),
          },
      .layers = layers,
  };
}

}  // namespace display_coordinator
