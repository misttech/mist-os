// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_EDID_TIMINGS_H_
#define SRC_GRAPHICS_DISPLAY_LIB_EDID_TIMINGS_H_

#include <lib/stdcompat/span.h>

#include <cinttypes>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace edid {

// TODO(https://fxbug.dev/135377): The type name violates the C++ style guide.
// Delete the struct once all the clients are migrated.
struct timing_params {
  uint32_t pixel_freq_khz;

  uint32_t horizontal_addressable;
  uint32_t horizontal_front_porch;
  uint32_t horizontal_sync_pulse;
  uint32_t horizontal_blanking;

  uint32_t vertical_addressable;
  uint32_t vertical_front_porch;
  uint32_t vertical_sync_pulse;
  uint32_t vertical_blanking;

  uint32_t flags;

  static constexpr uint32_t kPositiveHsync = (1 << 1);
  static constexpr uint32_t kPositiveVsync = (1 << 0);
  static constexpr uint32_t kInterlaced = (1 << 2);
  // Flag indicating alternating vblank lengths of |vertical_blanking|
  // and |vertical_blanking| + 1. The +1 is obtained by adding .5 to
  // the vfront and vback timings.
  static constexpr uint32_t kAlternatingVblank = (1 << 3);
  static constexpr uint32_t kDoubleClocked = (1 << 4);

  uint32_t vertical_refresh_e2;
};

// TODO(https://fxbug.dev/135377): The type name violates the C++ style guide.
// Delete the struct once all the clients are migrated.
using timing_params_t = timing_params;

namespace internal {

extern const cpp20::span<const display::DisplayTiming> kDmtDisplayTimings;
extern const cpp20::span<const display::DisplayTiming> kCtaDisplayTimings;

}  // namespace internal

display::DisplayTiming ToDisplayTiming(const timing_params& params);

}  // namespace edid

#endif  // SRC_GRAPHICS_DISPLAY_LIB_EDID_TIMINGS_H_
