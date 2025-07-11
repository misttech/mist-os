// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCHEDULING_TESTS_MOCKS_FRAME_SCHEDULER_MOCKS_H_
#define SRC_UI_SCENIC_LIB_SCHEDULING_TESTS_MOCKS_FRAME_SCHEDULER_MOCKS_H_

#include <deque>
#include <unordered_map>
#include <vector>

#include "src/ui/scenic/lib/scheduling/frame_scheduler.h"
#include "src/ui/scenic/lib/scheduling/vsync_timing.h"

namespace scheduling::test {

class MockFrameScheduler : public FrameScheduler {
 public:
  MockFrameScheduler() = default;

  // |FrameScheduler|
  void SetRenderContinuously(bool render_continuously) override;

  PresentId RegisterPresent(SessionId session_id, std::vector<zx::event> release_fences,
                            PresentId present_id) override;

  // |FrameScheduler|
  void ScheduleUpdateForSession(zx::time presentation_time, SchedulingIdPair id_pair,
                                bool squashable) override;

  // |FrameScheduler|
  std::vector<scheduling::FuturePresentationInfo> GetFuturePresentationInfos(
      zx::duration requested_prediction_span) override;

  // |FrameScheduler|
  void RemoveSession(SessionId session_id) override;

  // Testing only. Used for mock method callbacks.
  using OnSetRenderContinuouslyCallback = std::function<void(bool)>;
  using OnScheduleUpdateForSessionCallback = std::function<void(zx::time, SchedulingIdPair, bool)>;
  using OnGetFuturePresentationInfosCallback =
      std::function<std::vector<FuturePresentationInfo>(zx::duration requested_prediction_span)>;
  using RegisterPresentCallback = std::function<void(
      SessionId session_id, std::vector<zx::event> release_fences, PresentId present_id)>;
  using RemoveSessionCallback = std::function<void(SessionId session_id)>;

  // Testing only. Sets mock method callback.
  void set_set_render_continuously_callback(OnSetRenderContinuouslyCallback callback) {
    set_render_continuously_callback_ = callback;
  }

  // Testing only. Sets mock method callback.
  void set_schedule_update_for_session_callback(OnScheduleUpdateForSessionCallback callback) {
    schedule_update_for_session_callback_ = callback;
  }

  // Testing only. Sets mock method callback.
  void set_get_future_presentation_infos_callback(OnGetFuturePresentationInfosCallback callback) {
    get_future_presentation_infos_callback_ = callback;
  }

  // Testing only. Sets mock method callback.
  void set_register_present_callback(RegisterPresentCallback callback) {
    register_present_callback_ = callback;
  }

  // Testing only. Sets mock method callback.
  void set_remove_session_callback(RemoveSessionCallback callback) {
    remove_session_callback_ = callback;
  }

  void set_next_present_id(PresentId present_id) { next_present_id_ = present_id; }

 private:
  // Testing only. Mock method callbacks.
  OnSetRenderContinuouslyCallback set_render_continuously_callback_;
  OnScheduleUpdateForSessionCallback schedule_update_for_session_callback_;
  OnGetFuturePresentationInfosCallback get_future_presentation_infos_callback_;
  RegisterPresentCallback register_present_callback_;
  RemoveSessionCallback remove_session_callback_;

  PresentId next_present_id_;
};

}  // namespace scheduling::test

#endif  // SRC_UI_SCENIC_LIB_SCHEDULING_TESTS_MOCKS_FRAME_SCHEDULER_MOCKS_H_
