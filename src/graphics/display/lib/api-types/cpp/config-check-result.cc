// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

#include <cinttypes>
#include <string_view>
#include <type_traits>

namespace display {

static_assert(std::is_standard_layout_v<ConfigCheckResult>);
static_assert(std::is_trivially_assignable_v<ConfigCheckResult, ConfigCheckResult>);
static_assert(std::is_trivially_copyable_v<ConfigCheckResult>);
static_assert(std::is_trivially_copy_constructible_v<ConfigCheckResult>);
static_assert(std::is_trivially_destructible_v<ConfigCheckResult>);
static_assert(std::is_trivially_move_assignable_v<ConfigCheckResult>);
static_assert(std::is_trivially_move_constructible_v<ConfigCheckResult>);

// Ensure that the Banjo constants match the FIDL constants.
static_assert(ConfigCheckResult::kOk.ToBanjo() == CONFIG_CHECK_RESULT_OK);
static_assert(ConfigCheckResult::kEmptyConfig.ToBanjo() == CONFIG_CHECK_RESULT_EMPTY_CONFIG);
static_assert(ConfigCheckResult::kInvalidConfig.ToBanjo() == CONFIG_CHECK_RESULT_INVALID_CONFIG);
static_assert(ConfigCheckResult::kUnsupportedConfig.ToBanjo() ==
              CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG);
static_assert(ConfigCheckResult::kTooManyDisplays.ToBanjo() == CONFIG_CHECK_RESULT_TOO_MANY);
static_assert(ConfigCheckResult::kUnsupportedDisplayModes.ToBanjo() ==
              CONFIG_CHECK_RESULT_UNSUPPORTED_MODES);

std::string_view ConfigCheckResult::ToString() const {
  switch (result_) {
    case fuchsia_hardware_display_types::wire::ConfigResult::kOk:
      return "Ok";

    case fuchsia_hardware_display_types::wire::ConfigResult::kEmptyConfig:
      return "EmptyConfig";

    case fuchsia_hardware_display_types::wire::ConfigResult::kInvalidConfig:
      return "InvalidConfig";

    case fuchsia_hardware_display_types::wire::ConfigResult::kUnsupportedConfig:
      return "UnsupportedConfig";

    case fuchsia_hardware_display_types::wire::ConfigResult::kTooManyDisplays:
      return "TooManyDisplays";

    case fuchsia_hardware_display_types::wire::ConfigResult::kUnsupportedDisplayModes:
      return "UnsupportedDisplayModes";
  }

  ZX_DEBUG_ASSERT_MSG(false, "Invalid ConfigCheckResult value: %" PRIu32, ValueForLogging());
  return "(invalid value)";
}

}  // namespace display
