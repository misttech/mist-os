// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/engine-info.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>

namespace display {

// Ensure that the `EngineInfo` constants match the FIDL constants.
static_assert(fuchsia_hardware_display_engine::wire::kMaxAllowedMaxLayerCount ==
              EngineInfo::kMaxAllowedMaxLayerCount);
static_assert(fuchsia_hardware_display_engine::wire::kMaxAllowedMaxConnectedDisplayCount ==
              EngineInfo::kMaxAllowedMaxConnectedDisplayCount);

}  // namespace display
