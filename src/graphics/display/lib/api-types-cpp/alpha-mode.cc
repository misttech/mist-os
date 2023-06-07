// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/alpha-mode.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <zircon/assert.h>

#include <limits>
#include <type_traits>

namespace display {

AlphaMode ToAlphaMode(fuchsia_hardware_display::wire::AlphaMode alpha_mode_fidl) {
  return alpha_mode_fidl;
}

AlphaMode ToAlphaMode(alpha_t alpha_banjo) {
  switch (alpha_banjo) {
    case ALPHA_DISABLE:
      return AlphaMode::kDisable;
    case ALPHA_PREMULTIPLIED:
      return AlphaMode::kPremultiplied;
    case ALPHA_HW_MULTIPLY:
      return AlphaMode::kHwMultiply;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid banjo Alpha %d", static_cast<int>(alpha_banjo));
      return AlphaMode::kDisable;
  }
}

fuchsia_hardware_display::wire::AlphaMode ToFidlAlphaMode(AlphaMode alpha_mode) {
  return alpha_mode;
}

alpha_t ToBanjoAlpha(AlphaMode alpha_mode) {
  switch (alpha_mode) {
    case AlphaMode::kDisable:
      return ALPHA_DISABLE;
    case AlphaMode::kPremultiplied:
      return ALPHA_PREMULTIPLIED;
    case AlphaMode::kHwMultiply:
      return ALPHA_HW_MULTIPLY;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid AlphaMode %d", static_cast<int>(alpha_mode));
      return ALPHA_DISABLE;
  }
}

}  // namespace display
