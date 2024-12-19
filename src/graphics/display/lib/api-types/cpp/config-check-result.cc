// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

namespace display {

// Ensure that the Banjo constants match the FIDL constants.
static_assert(ConfigCheckResult::kOk.ToBanjo() == CONFIG_CHECK_RESULT_OK);
static_assert(ConfigCheckResult::kInvalidConfig.ToBanjo() == CONFIG_CHECK_RESULT_INVALID_CONFIG);
static_assert(ConfigCheckResult::kUnsupportedConfig.ToBanjo() ==
              CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG);
static_assert(ConfigCheckResult::kTooManyDisplays.ToBanjo() == CONFIG_CHECK_RESULT_TOO_MANY);
static_assert(ConfigCheckResult::kUnsupportedDisplayModes.ToBanjo() ==
              CONFIG_CHECK_RESULT_UNSUPPORTED_MODES);

}  // namespace display
