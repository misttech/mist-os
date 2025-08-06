// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_BANJO_FIDL_CONVERSION_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_BANJO_FIDL_CONVERSION_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/fdf/cpp/arena.h>

namespace display_coordinator {

// Converts a Banjo `display_config_t` to a FIDL
// `fuchsia_hardware_display_engine::wire::DisplayConfig`.
//
// This function allocates memory for the layers from the provided arena.
fuchsia_hardware_display_engine::wire::DisplayConfig ToFidlDisplayConfig(
    const display_config_t& banjo_config, fdf::Arena& arena);

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_BANJO_FIDL_CONVERSION_H_
