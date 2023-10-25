// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/edid/timings.h"

#include <array>
#include <cstddef>

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
}  // namespace edid
