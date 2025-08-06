// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/alpha-mode.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

#include <cinttypes>
#include <string_view>
#include <type_traits>

namespace display {

static_assert(std::is_standard_layout_v<AlphaMode>);
static_assert(std::is_trivially_assignable_v<AlphaMode, AlphaMode>);
static_assert(std::is_trivially_copyable_v<AlphaMode>);
static_assert(std::is_trivially_copy_constructible_v<AlphaMode>);
static_assert(std::is_trivially_destructible_v<AlphaMode>);
static_assert(std::is_trivially_move_assignable_v<AlphaMode>);
static_assert(std::is_trivially_move_constructible_v<AlphaMode>);

// Ensure that the Banjo constants match the FIDL constants.
static_assert(AlphaMode::kDisable.ToBanjo() == ALPHA_DISABLE);
static_assert(AlphaMode::kPremultiplied.ToBanjo() == ALPHA_PREMULTIPLIED);
static_assert(AlphaMode::kHwMultiply.ToBanjo() == ALPHA_HW_MULTIPLY);

std::string_view AlphaMode::ToString() const {
  switch (alpha_mode_) {
    case fuchsia_hardware_display_types::wire::AlphaMode::kDisable:
      return "Disable";
    case fuchsia_hardware_display_types::wire::AlphaMode::kPremultiplied:
      return "Premultiplied";
    case fuchsia_hardware_display_types::wire::AlphaMode::kHwMultiply:
      return "HwMultiply";
  }

  ZX_DEBUG_ASSERT_MSG(false, "Invalid AlphaMode value: %" PRIu32, ValueForLogging());
  return "(invalid value)";
}

}  // namespace display
