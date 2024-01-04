// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/edid/timings.h"

#include <array>
#include <cstddef>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/edid/cta-timing.h"
#include "src/graphics/display/lib/edid/dmt-timing.h"

namespace edid {
namespace internal {

constexpr std::array<timing_params_t, kDmtTimings.size()> kDmtTimingParams = [] {
  std::array<timing_params_t, kDmtTimings.size()> params = {};
  for (size_t i = 0; i < kDmtTimings.size(); i++) {
    params[i] = ToTimingParams(kDmtTimings[i]);
  }
  return params;
}();

const timing_params_t* dmt_timings = kDmtTimingParams.data();
const uint32_t dmt_timings_count = kDmtTimingParams.size();

constexpr std::array<timing_params_t, kCtaTimings.size()> kCtaTimingParams = [] {
  std::array<timing_params_t, kCtaTimings.size()> params = {};
  for (size_t i = 0; i < kCtaTimings.size(); i++) {
    params[i] = ToTimingParams(kCtaTimings[i]);
  }
  return params;
}();

const timing_params_t* cea_timings = kCtaTimingParams.data();
const uint32_t cea_timings_count = kCtaTimingParams.size();

}  // namespace internal

display::DisplayTiming ToDisplayTiming(const timing_params& timing_params) {
  // A valid display_mode_t guarantees that both horizontal_front_porch and
  // horizontal_sync_pulse are no more than kMaxTimingValue, so
  // (horizontal_front_porch + horizontal_sync_pulse) won't overflow.
  //
  // It also guarantees that horizontal_blanking >= horizontal_front_porch +
  // horizontal_sync_pulse, so horizontal_blanking - (horizontal_front_porch +
  // horizontal_sync_pulse) won't overflow and will fit in [0, kMaxTimingValue]
  // -- so we can use int32_t.
  int32_t horizontal_back_porch_px = static_cast<int32_t>(
      timing_params.horizontal_blanking -
      (timing_params.horizontal_front_porch + timing_params.horizontal_sync_pulse));

  // An argument similar to the one above can be used to show that the vertical
  // back porch calculations do not cause UB.
  int32_t vertical_back_porch_lines =
      static_cast<int32_t>(timing_params.vertical_blanking - (timing_params.vertical_front_porch +
                                                              timing_params.vertical_sync_pulse));

  const display::DisplayTiming display_timing = {
      .horizontal_active_px = static_cast<int32_t>(timing_params.horizontal_addressable),
      .horizontal_front_porch_px = static_cast<int32_t>(timing_params.horizontal_front_porch),
      .horizontal_sync_width_px = static_cast<int32_t>(timing_params.horizontal_sync_pulse),
      .horizontal_back_porch_px = horizontal_back_porch_px,
      .vertical_active_lines = static_cast<int32_t>(timing_params.vertical_addressable),
      .vertical_front_porch_lines = static_cast<int32_t>(timing_params.vertical_front_porch),
      .vertical_sync_width_lines = static_cast<int32_t>(timing_params.vertical_sync_pulse),
      .vertical_back_porch_lines = vertical_back_porch_lines,
      .pixel_clock_frequency_khz = static_cast<int32_t>(timing_params.pixel_freq_khz),
      .fields_per_frame = (timing_params.flags & edid::timing_params_t::kInterlaced)
                              ? display::FieldsPerFrame::kInterlaced
                              : display::FieldsPerFrame::kProgressive,
      .hsync_polarity = (timing_params.flags & edid::timing_params_t::kPositiveHsync)
                            ? display::SyncPolarity::kPositive
                            : display::SyncPolarity::kNegative,
      .vsync_polarity = (timing_params.flags & edid::timing_params_t::kPositiveVsync)
                            ? display::SyncPolarity::kPositive
                            : display::SyncPolarity::kNegative,
      .vblank_alternates = (timing_params.flags & edid::timing_params_t::kAlternatingVblank) != 0,
      .pixel_repetition = (timing_params.flags & edid::timing_params_t::kDoubleClocked) ? 1 : 0,
  };

  return display_timing;
}

}  // namespace edid
