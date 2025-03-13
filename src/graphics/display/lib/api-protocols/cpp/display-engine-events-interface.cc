// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-interface.h"

#include <lib/stdcompat/span.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display {

void DisplayEngineEventsInterface::OnDisplayAdded(
    display::DisplayId display_id, cpp20::span<const display::ModeAndId> preferred_modes,
    cpp20::span<const display::PixelFormat> pixel_formats) {
  cpp20::span<const uint8_t> empty_edid_bytes;
  OnDisplayAdded(display_id, preferred_modes, empty_edid_bytes, pixel_formats);
}

}  // namespace display
