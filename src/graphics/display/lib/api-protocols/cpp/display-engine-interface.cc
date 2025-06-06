// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"

#include <zircon/assert.h>

#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"

namespace display {

// TODO(https://fxbug.dev/422844790): Remove this overload after drivers are migrated.
display::ConfigCheckResult DisplayEngineInterface::CheckConfiguration(
    display::DisplayId display_id, display::ModeId display_mode_id,
    cpp20::span<const display::DriverLayer> layers,
    cpp20::span<display::LayerCompositionOperations> layer_composition_operations) {
  ZX_DEBUG_ASSERT_MSG(false, "Drivers must override one CheckConfiguration() overload!");
  return display::ConfigCheckResult::kUnsupportedConfig;
}

// TODO(https://fxbug.dev/422844790): Remove the default implementation after
// all drivers override this method.
display::ConfigCheckResult DisplayEngineInterface::CheckConfiguration(
    display::DisplayId display_id, display::ModeId display_mode_id,
    cpp20::span<const display::DriverLayer> layers) {
  display::LayerCompositionOperations layer0_composition_operations;
  cpp20::span<display::LayerCompositionOperations> layer_composition_operations(
      &layer0_composition_operations, 1);
  return CheckConfiguration(display_id, display_mode_id, layers, layer_composition_operations);
}

zx::result<> DisplayEngineInterface::SetDisplayPower(display::DisplayId display_id, bool power_on) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> DisplayEngineInterface::StartCapture(display::DriverCaptureImageId capture_image_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> DisplayEngineInterface::ReleaseCapture(
    display::DriverCaptureImageId capture_image_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> DisplayEngineInterface::SetMinimumRgb(uint8_t minimum_rgb) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace display
