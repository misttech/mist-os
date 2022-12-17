// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/scheduling/tests/mocks/frame_scheduler_mocks.h"

#include <lib/async/cpp/time.h>
#include <lib/async/default.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace scheduling::test {

PresentId MockFrameScheduler::RegisterPresent(SessionId session_id,
                                              std::vector<zx::event> release_fences,
                                              PresentId present_id) {
  if (register_present_callback_) {
    register_present_callback_(session_id, std::move(release_fences), present_id);
  }

  return present_id != 0 ? present_id : next_present_id_++;
}

void MockFrameScheduler::SetRenderContinuously(bool render_continuously) {
  if (set_render_continuously_callback_) {
    set_render_continuously_callback_(render_continuously);
  }
}

void MockFrameScheduler::ScheduleUpdateForSession(zx::time presentation_time,
                                                  SchedulingIdPair id_pair, bool squashable) {
  if (schedule_update_for_session_callback_) {
    schedule_update_for_session_callback_(presentation_time, id_pair, squashable);
  }
}

std::vector<scheduling::FuturePresentationInfo> MockFrameScheduler::GetFuturePresentationInfos(
    zx::duration requested_prediction_span) {
  if (get_future_presentation_infos_callback_) {
    return get_future_presentation_infos_callback_(requested_prediction_span);
  } else {
    return {};
  }
}

void MockFrameScheduler::RemoveSession(SessionId session_id) {
  if (remove_session_callback_) {
    remove_session_callback_(session_id);
  }
}

}  // namespace scheduling::test
