// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/scheduling/frame_predictor.h"

#include <lib/syslog/cpp/macros.h>

#include <algorithm>

namespace scheduling {

// static
zx::time FramePredictor::ComputeNextVsyncTime(zx::time base_vsync_time, zx::duration vsync_interval,
                                              zx::time min_vsync_time) {
  FX_DCHECK(vsync_interval.get() > 0);
  // If the base sync time is greater than or equal to the minimum acceptable
  // sync time, just return it.
  // Note: in practice, these numbers are unlikely to be identical. The "equal to"
  // comparison is necessary for tests, which have much tighter control on time.
  if (base_vsync_time >= min_vsync_time) {
    return base_vsync_time;
  }

  // The "-1" is so that `ComputeNextVsyncTime(5, 10, 15)` returns 15 instead of 25.
  // The latter would be surprising, because the if-statement above causes
  // `ComputeNextVsyncTime(5, 10, 5)` to return 5.
  const int64_t num_intervals =
      (min_vsync_time.get() - base_vsync_time.get() - 1) / vsync_interval.get();
  return base_vsync_time + (vsync_interval * (num_intervals + 1));
}

// static
PredictedTimes FramePredictor::ComputePredictionFromDuration(
    PredictionRequest request, zx::duration frame_preparation_duration) {
  // Calculate minimum time this would sync to. It is last vsync time plus half
  // a vsync-interval (to allow for jitter for the VSYNC signal), or the current
  // time plus the expected render time, whichever is larger, so we know we have
  // enough time to render for that sync.
  const zx::time min_presentation_time =
      std::max((request.last_vsync_time + (request.vsync_interval / 2)),
               (request.now + frame_preparation_duration));
  const zx::time target_vsync_time =
      ComputeNextVsyncTime(request.last_vsync_time, request.vsync_interval, min_presentation_time);

  // Ensure the requested presentation time is current.
  zx::time target_presentation_time = std::max(request.requested_presentation_time, request.now);

  // Compute the next presentation time from the target vsync time (inclusive),
  // that is at least the current requested present time.
  target_presentation_time =
      ComputeNextVsyncTime(target_vsync_time, request.vsync_interval, target_presentation_time);

  // Find time the client should latch and start rendering in order to
  // frame in time for the target present.
  const zx::time latch_point = target_presentation_time - frame_preparation_duration;

  return {.latch_point_time = latch_point, .presentation_time = target_presentation_time};
}

}  // namespace scheduling
