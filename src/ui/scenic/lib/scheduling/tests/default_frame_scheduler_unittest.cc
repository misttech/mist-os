// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/scheduling/default_frame_scheduler.h"

#include <lib/async/cpp/time.h>
#include <lib/async/default.h>

#include <gmock/gmock.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/ui/scenic/lib/scheduling/windowed_frame_predictor.h"
#include "src/ui/scenic/lib/utils/helpers.h"

namespace scheduling::test {

class FrameSchedulerTest : public ::gtest::TestLoopFixture {
 protected:
  FrameSchedulerTest()
      : scheduler_(std::make_unique<WindowedFramePredictor>(
            DefaultFrameScheduler::kMinPredictedFrameDuration,
            DefaultFrameScheduler::kInitialRenderDuration,
            DefaultFrameScheduler::kInitialUpdateDuration)) {
    vsync_timing_ = std::make_shared<VsyncTiming>();

    // Set up default vsync values.
    // Needs to be big enough so that FrameScheduler can always fit a latch point
    // in the frame.
    const auto vsync_interval = zx::msec(100);
    vsync_timing_->set_vsync_interval(vsync_interval);
    vsync_timing_->set_last_vsync_time(zx::time(0));

    scheduler_.Initialize(
        vsync_timing_,
        /*update_sessions*/
        [this](auto& sessions_to_update, auto trace_id, auto fences_from_previous_presents) {
          ++update_sessions_call_count_;
          last_sessions_to_update_ = sessions_to_update;

          last_received_fences_ = std::move(fences_from_previous_presents);
        },
        /*on_cpu_work_done*/
        [this]() { cpu_work_done_count_++; },
        /*on_frame_presented*/
        [this](auto latched_times, auto present_times) {
          last_latched_times_ = latched_times;
          last_presented_time_ = present_times.presented_time;
          on_frame_presented_call_count_++;
        },
        /*render_scheduled_frame*/
        [this](auto frame_number, auto presentation_time, auto callback) {
          FX_CHECK(!frame_presented_callback_.has_value())
              << "Currently only support a single frame in flight.";
          frame_presented_callback_ = std::move(callback);
        });
  }

  Timestamps CreateTimestamps() {
    return Timestamps{
        .render_done_time = Now(),
        .actual_presentation_time = Now(),
    };
  }

  // Schedule an update on the frame scheduler.
  void ScheduleUpdate(SessionId session_id, zx::time presentation_time,
                      std::vector<zx::event> release_fences = {}, bool squashable = true) {
    scheduling::PresentId present_id =
        scheduler_.RegisterPresent(session_id, std::move(release_fences));
    scheduler_.ScheduleUpdateForSession(
        presentation_time, {.session_id = session_id, .present_id = present_id}, squashable);
  }

  void FireFramePresentedCallback(std::optional<Timestamps> timestamps = std::nullopt) {
    frame_presented_callback_.value()(timestamps.value_or(CreateTimestamps()));
    frame_presented_callback_.reset();
  }

  // This function runs a single frame through the scheduler_, updater, and renderer. It performs a
  // positive test for timing behavior, confirming that the requested update (triggered at
  // |presentation_time|) is not triggered before |early_time|, but has been triggered after
  // |update_time|.
  void SingleRenderTest(zx::time presentation_time, zx::time early_time, zx::time update_time) {
    constexpr SessionId kSessionId = 1;

    EXPECT_EQ(update_sessions_call_count_, 0u);
    EXPECT_FALSE(frame_presented_callback_.has_value());
    EXPECT_EQ(cpu_work_done_count_, 0u);

    ScheduleUpdate(kSessionId, presentation_time);

    EXPECT_GE(early_time, Now());
    test_loop().RunUntil(early_time);

    EXPECT_EQ(update_sessions_call_count_, 0u);
    EXPECT_FALSE(frame_presented_callback_.has_value());
    EXPECT_EQ(cpu_work_done_count_, 0u);

    EXPECT_GE(update_time, Now());
    test_loop().RunUntil(update_time);

    // Present should have been scheduled and handled.
    EXPECT_EQ(update_sessions_call_count_, 1u);
    EXPECT_TRUE(frame_presented_callback_.has_value());
    EXPECT_EQ(cpu_work_done_count_, 1u);

    // Wait for a very long time.
    test_loop().RunFor(zx::sec(10));

    // No further render calls should have been made.
    EXPECT_EQ(update_sessions_call_count_, 1u);
    EXPECT_TRUE(frame_presented_callback_.has_value());
    EXPECT_EQ(cpu_work_done_count_, 1u);

    // End the pending frame.
    EXPECT_EQ(on_frame_presented_call_count_, 0u);
    FireFramePresentedCallback();
    EXPECT_FALSE(frame_presented_callback_.has_value());
    EXPECT_EQ(on_frame_presented_call_count_, 1u);
    ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
    EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 1u);
    EXPECT_EQ(cpu_work_done_count_, 1u);

    // Wait for a very long time.
    test_loop().RunFor(zx::sec(10));

    // No further render calls should have been made.
    EXPECT_EQ(update_sessions_call_count_, 1u);
    EXPECT_FALSE(frame_presented_callback_.has_value());
    EXPECT_EQ(cpu_work_done_count_, 1u);
    EXPECT_EQ(on_frame_presented_call_count_, 1u);
  }

  DefaultFrameScheduler scheduler_;

  uint64_t update_sessions_call_count_ = 0;
  uint64_t on_frame_presented_call_count_ = 0;
  uint64_t cpu_work_done_count_ = 0;
  std::unordered_map<scheduling::SessionId, scheduling::PresentId> last_sessions_to_update_;
  std::unordered_map<scheduling::SessionId,
                     std::map<scheduling::PresentId, /*latched_time*/ zx::time>>
      last_latched_times_;
  zx::time last_presented_time_;

  std::optional<FramePresentedCallback> frame_presented_callback_;
  std::vector<zx::event> last_received_fences_;

  std::shared_ptr<VsyncTiming> vsync_timing_;
};

TEST_F(FrameSchedulerTest, PresentTimeZero_ShouldBeScheduledBeforeNextVsync) {
  SingleRenderTest(zx::time(0), zx::time(0), zx::time(0) + vsync_timing_->vsync_interval());
}

TEST_F(FrameSchedulerTest, PresentBiggerThanNextVsync_ShouldBeScheduledAfterNextVsync) {
  // Schedule an update for in between the next two vsyncs.
  const auto vsync_interval = vsync_timing_->vsync_interval();
  const zx::time early_time = vsync_timing_->last_vsync_time() + vsync_interval;
  const zx::time update_time = vsync_timing_->last_vsync_time() + vsync_interval * 2;
  const zx::time presentation_time = early_time + (update_time - early_time) / 2;

  SingleRenderTest(presentation_time, early_time, update_time);
}

TEST_F(FrameSchedulerTest, SinglePresent_ShouldGetSingleRenderCallExactlyOnTime) {
  // Set the LastVsyncTime arbitrarily in the future.
  //
  // We want to test our ability to schedule a frame "next time" given an arbitrary start,
  // vs in a certain duration from Now() = 0, so this makes that distinction clear.
  const auto vsync_interval = vsync_timing_->vsync_interval();
  const zx::time early_time = vsync_timing_->last_vsync_time() + vsync_interval * 6;
  const zx::time update_time = vsync_timing_->last_vsync_time() + vsync_interval * 7;
  const zx::time presentation_time = update_time;
  vsync_timing_->set_last_vsync_time(early_time);

  SingleRenderTest(presentation_time, early_time, update_time);
}

TEST_F(FrameSchedulerTest, PresentsForTheSameFrame_ShouldGetSquashedAndSingleRenderCall) {
  // Schedule an extra update for now.
  constexpr SessionId kSessionId = 2;
  const zx::time now = Now();
  ScheduleUpdate(kSessionId, now);
  ScheduleUpdate(kSessionId, now);

  test_loop().RunUntil(now + vsync_timing_->vsync_interval());

  // Present should have been scheduled and applied.
  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_EQ(on_frame_presented_call_count_, 0u);

  // Present the frame.
  FireFramePresentedCallback();

  // The two updates should be squashed and presented together.
  EXPECT_EQ(on_frame_presented_call_count_, 1u);
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 2u);
}

TEST_F(FrameSchedulerTest, SquashedPresents_ShouldScheduleForInitialPresent) {
  // Schedule two updates. The first with a later requested_presentation_time than the second. They
  // should be squashed.
  constexpr SessionId kSessionId = 1;
  ScheduleUpdate(kSessionId,
                 zx::time(static_cast<zx_time_t>(
                     1.5 * static_cast<double>(vsync_timing_->vsync_interval().get()))));
  ScheduleUpdate(kSessionId, zx::time(0));

  // Run loop past when a frame would have been scheduled in case update #2 was used.
  // Observe no attempt to apply changes.
  const zx::time now = Now();
  test_loop().RunUntil(now + vsync_timing_->vsync_interval());
  EXPECT_EQ(update_sessions_call_count_, 0u);

  // Wait for the requested time for update 1 to pass. Should now see an attempted update.
  test_loop().RunUntil(now + zx::duration(2 * vsync_timing_->vsync_interval().get()));
  EXPECT_EQ(update_sessions_call_count_, 1u);

  // Both updates should have been applied.
  FireFramePresentedCallback();
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 2u);
}

TEST_F(FrameSchedulerTest, UnsquashablePresents_ShouldNeverBeSquashed) {
  EXPECT_EQ(update_sessions_call_count_, 0u);

  // Schedule four updates with the same presentation time, but different squashability.
  constexpr SessionId kSessionId = 1;
  ScheduleUpdate(kSessionId, zx::time(0), /*release_fences*/ {}, /*squashable=*/false);
  ScheduleUpdate(kSessionId, zx::time(0), /*release_fences*/ {}, /*squashable=*/false);
  ScheduleUpdate(kSessionId, zx::time(0), /*release_fences*/ {}, /*squashable=*/true);
  ScheduleUpdate(kSessionId, zx::time(0), /*release_fences*/ {}, /*squashable=*/false);

  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  // Present should have been scheduled and applied.
  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_EQ(on_frame_presented_call_count_, 0u);

  // Present the frame.
  FireFramePresentedCallback();

  // Only one update should have been applied.
  EXPECT_EQ(on_frame_presented_call_count_, 1u);
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 1u);

  // Next frame should also apply a single one.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 2u);
  FireFramePresentedCallback();
  EXPECT_EQ(on_frame_presented_call_count_, 2u);
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 1u);

  // Third update is squashable, so next frame should contain update 3+4.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 3u);
  FireFramePresentedCallback();
  EXPECT_EQ(on_frame_presented_call_count_, 3u);
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 2u);

  // All updates should have been completed.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 3u);
}

TEST_F(FrameSchedulerTest, PresentsForDifferentFrames_ShouldGetSeparateRenderCalls) {
  constexpr SessionId kSessionId = 1;

  const zx::time now = Now();
  EXPECT_EQ(now, vsync_timing_->last_vsync_time());

  EXPECT_EQ(update_sessions_call_count_, 0u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // Schedule an update for now.
  ScheduleUpdate(kSessionId, now);

  // Schedule an update for in between the next two vsyncs.
  const auto vsync_interval = vsync_timing_->vsync_interval();
  const zx::time early_time = vsync_timing_->last_vsync_time() + vsync_interval;
  const zx::time update_time = vsync_timing_->last_vsync_time() + vsync_interval * 2;
  const zx::time presentation_time = early_time + (update_time - early_time) / 2;

  ScheduleUpdate(kSessionId, presentation_time);

  EXPECT_EQ(update_sessions_call_count_, 0u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // Wait for one vsync period.
  RunLoopUntil(early_time);

  // First Present should have been scheduled and applied.
  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_TRUE(frame_presented_callback_.has_value());
  EXPECT_EQ(on_frame_presented_call_count_, 0u);

  FireFramePresentedCallback();
  // First Present should have been completed.
  EXPECT_FALSE(frame_presented_callback_.has_value());
  EXPECT_EQ(on_frame_presented_call_count_, 1u);
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 1u);

  // Wait for one more vsync period.
  RunLoopUntil(update_time);

  // Second Present should have been scheduled and applied.
  EXPECT_EQ(update_sessions_call_count_, 2u);
  EXPECT_TRUE(frame_presented_callback_.has_value());
  EXPECT_EQ(on_frame_presented_call_count_, 1u);

  FireFramePresentedCallback();
  // Second Present should have been completed.
  EXPECT_FALSE(frame_presented_callback_.has_value());
  EXPECT_EQ(on_frame_presented_call_count_, 2u);
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 1u);
}

TEST_F(FrameSchedulerTest, SecondPresentDuringRender_ShouldApplyUpdatesAndReschedule) {
  constexpr SessionId kSessionId = 1;

  EXPECT_EQ(update_sessions_call_count_, 0u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // Schedule an update for now.
  zx::time now = Now();
  ScheduleUpdate(kSessionId, now);

  // Wait for one vsync period.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // Schedule another update for now.
  ScheduleUpdate(kSessionId, now);
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  // Updates should be applied, but not rendered.
  EXPECT_EQ(update_sessions_call_count_, 2u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // End previous frame.
  FireFramePresentedCallback();
  EXPECT_FALSE(frame_presented_callback_.has_value());

  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  // Second render should have occurred.
  EXPECT_TRUE(frame_presented_callback_.has_value());
}

TEST_F(FrameSchedulerTest, SignalSuccessfulPresentCallbackOnlyWhenFramePresented) {
  constexpr SessionId kSessionId = 1;

  EXPECT_EQ(update_sessions_call_count_, 0u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // Schedule an update for now.
  zx::time now = Now();
  ScheduleUpdate(kSessionId, now);

  // Wait for one vsync period.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // Schedule another update.
  ScheduleUpdate(kSessionId, now);
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  // Next render doesn't trigger until the previous render is finished.
  EXPECT_EQ(update_sessions_call_count_, 2u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // Drop frame #0. This should not trigger a frame presented signal.
  FireFramePresentedCallback(
      Timestamps{.render_done_time = kTimeDropped, .actual_presentation_time = kTimeDropped});
  EXPECT_FALSE(frame_presented_callback_.has_value());
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_TRUE(frame_presented_callback_.has_value());
  EXPECT_EQ(on_frame_presented_call_count_, 0u);

  // Presenting frame #1 should trigger frame presented signal for both updates.
  FireFramePresentedCallback();
  EXPECT_EQ(on_frame_presented_call_count_, 1u);
  EXPECT_EQ(last_latched_times_.size(), 1u);
  ASSERT_EQ(last_latched_times_.count(kSessionId), 1u);
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 2u);
}

TEST_F(FrameSchedulerTest, FailedUpdateWithRender_ShouldNotCrash) {
  constexpr SessionId kSessionId1 = 1;
  constexpr SessionId kSessionId2 = 2;

  uint64_t present_counts[2] = {0, 0};
  ScheduleUpdate(kSessionId1, Now());
  ScheduleUpdate(kSessionId2, Now());

  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_TRUE(frame_presented_callback_.has_value());
  EXPECT_EQ(on_frame_presented_call_count_, 0u);
  EXPECT_NO_FATAL_FAILURE(FireFramePresentedCallback());
  EXPECT_EQ(on_frame_presented_call_count_, 1u);
  // TODO(): The session with the failed update should not receive an OnFramePresented call.
  EXPECT_EQ(last_latched_times_.size(), 2u);
  EXPECT_TRUE(last_latched_times_.count(kSessionId1));
  EXPECT_TRUE(last_latched_times_.count(kSessionId2));
}

TEST_F(FrameSchedulerTest, NoOpUpdateWithSecondPendingUpdate_ShouldBeRescheduled) {
  constexpr SessionId kSessionId = 1;

  EXPECT_EQ(update_sessions_call_count_, 0u);

  ScheduleUpdate(kSessionId, Now() + vsync_timing_->vsync_interval());
  ScheduleUpdate(kSessionId, Now() + (vsync_timing_->vsync_interval() + zx::duration(1)));

  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 1u);

  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 2u);
}

TEST_F(FrameSchedulerTest, LongRenderTime_ShouldTriggerAReschedule_WithALatePresent) {
  constexpr SessionId kSessionId = 1;

  // Guarantee the vsync interval here is what we expect.
  zx::duration interval = zx::msec(100);
  vsync_timing_->set_vsync_interval(interval);
  EXPECT_EQ(0, Now().get());

  // Schedule a frame
  ScheduleUpdate(kSessionId, zx::time(0));

  EXPECT_EQ(update_sessions_call_count_, 0u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // Trigger an update
  auto update_time = zx::time(vsync_timing_->last_vsync_time() + vsync_timing_->vsync_interval());

  // Go to vsync.
  RunLoopUntil(update_time);
  vsync_timing_->set_last_vsync_time(Now());

  // Present should have been scheduled and handled.
  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // End the frame, more than halfway through the vsync, so that the next update cannot complete in
  // time, given prediction.
  RunLoopFor(zx::msec(91));
  FireFramePresentedCallback(
      Timestamps{.render_done_time = Now(), .actual_presentation_time = Now()});
  EXPECT_FALSE(frame_presented_callback_.has_value());

  ScheduleUpdate(kSessionId, zx::time(0));

  // Go to vsync.
  RunLoopUntil(zx::time(vsync_timing_->last_vsync_time() + vsync_timing_->vsync_interval()));
  vsync_timing_->set_last_vsync_time(Now());

  // Nothing should have been scheduled yet.
  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // Wait for one more vsync period.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_EQ(update_sessions_call_count_, 2u);
  EXPECT_TRUE(frame_presented_callback_.has_value());
}

TEST_F(FrameSchedulerTest, SinglePredictedPresentation_ShouldBeReasonable) {
  zx::time next_vsync = vsync_timing_->last_vsync_time() + vsync_timing_->vsync_interval();

  // Ask for a prediction for one frame into the future.
  std::vector<scheduling::FuturePresentationInfo> predicted_presents =
      scheduler_.GetFuturePresentationInfos(zx::duration(0));

  EXPECT_GE(predicted_presents.size(), 1u);
  EXPECT_EQ(predicted_presents[0].presentation_time, next_vsync);

  for (size_t i = 0; i < predicted_presents.size(); i++) {
    auto current = std::move(predicted_presents[i]);
    EXPECT_LT(current.latch_point, current.presentation_time);
    EXPECT_GE(current.latch_point, Now());
  }
}

TEST_F(FrameSchedulerTest, ArbitraryPredictedPresentation_ShouldBeReasonable) {
  // The main and only difference between this test and
  // "SinglePredictedPresentation_ShouldBeReasonable" above is that we advance the clock before
  // asking for a prediction, to ensure that GetPredictions() works in a more general sense.

  // Advance the clock to vsync1.
  zx::time vsync0 = vsync_timing_->last_vsync_time();
  zx::time vsync1 = vsync0 + vsync_timing_->vsync_interval();
  zx::time vsync = vsync1 + vsync_timing_->vsync_interval();

  EXPECT_GT(vsync_timing_->vsync_interval(), zx::duration(0));
  EXPECT_EQ(vsync0, Now());

  RunLoopUntil(vsync1);

  // Ask for a prediction.
  std::vector<scheduling::FuturePresentationInfo> predicted_presents =
      scheduler_.GetFuturePresentationInfos(zx::duration(0));

  EXPECT_GE(predicted_presents.size(), 1u);
  EXPECT_EQ(predicted_presents[0].presentation_time, vsync);

  for (size_t i = 0; i < predicted_presents.size(); i++) {
    auto current = std::move(predicted_presents[i]);
    EXPECT_LT(current.latch_point, current.presentation_time);
    EXPECT_GE(current.latch_point, Now());
  }
}

TEST_F(FrameSchedulerTest, MultiplePredictedPresentations_ShouldBeReasonable) {
  zx::time vsync0 = vsync_timing_->last_vsync_time();
  zx::time vsync1 = vsync0 + vsync_timing_->vsync_interval();
  zx::time vsync = vsync1 + vsync_timing_->vsync_interval();
  zx::time vsync3 = vsync + vsync_timing_->vsync_interval();
  zx::time vsync4 = vsync3 + vsync_timing_->vsync_interval();

  // What we really want is a positive difference between each vsync.
  EXPECT_GT(vsync_timing_->vsync_interval(), zx::duration(0));

  // Ask for a prediction a few frames into the future.
  std::vector<scheduling::FuturePresentationInfo> predicted_presents =
      scheduler_.GetFuturePresentationInfos(zx::duration((vsync4 - vsync0).get()));

  // Expect at least one frame of prediction.
  EXPECT_GE(predicted_presents.size(), 1u);

  auto past_prediction = std::move(predicted_presents[0]);

  for (size_t i = 0; i < predicted_presents.size(); i++) {
    auto current = std::move(predicted_presents[i]);
    EXPECT_LT(current.latch_point, current.presentation_time);
    EXPECT_GE(current.latch_point, Now());

    if (i > 0)
      EXPECT_LT(past_prediction.presentation_time, current.presentation_time);

    past_prediction = std::move(current);
  }
}

TEST_F(FrameSchedulerTest, InfinitelyLargePredictionRequest_ShouldBeTruncated) {
  zx::time next_vsync = vsync_timing_->last_vsync_time() + vsync_timing_->vsync_interval();

  // Ask for an extremely large prediction duration.
  std::vector<scheduling::FuturePresentationInfo> predicted_presents =
      scheduler_.GetFuturePresentationInfos(zx::duration(INTMAX_MAX));

  constexpr static const uint64_t kOverlyLargeRequestCount = 100u;

  EXPECT_LE(predicted_presents.size(), kOverlyLargeRequestCount);
  EXPECT_EQ(predicted_presents[0].presentation_time, next_vsync);

  for (size_t i = 0; i < predicted_presents.size(); i++) {
    auto current = std::move(predicted_presents[i]);
    EXPECT_LT(current.latch_point, current.presentation_time);
    EXPECT_GE(current.latch_point, Now());
  }
}

// Tests whether the OnPresented is called at the correct times with the correct
// data.
TEST_F(FrameSchedulerTest, SessionUpdater_OnPresented_Test) {
  constexpr SessionId kSessionId1 = 1;
  constexpr SessionId kSessionId2 = 2;

  // Schedule a couple of updates, all of which should be handled this frame.
  ScheduleUpdate(kSessionId1, zx::time(0));
  ScheduleUpdate(kSessionId1, zx::time(0));
  ScheduleUpdate(kSessionId1, zx::time(0));
  ScheduleUpdate(kSessionId2, zx::time(0));

  // Schedule updates for next frame.
  ScheduleUpdate(kSessionId1,
                 zx::time(0) + zx::duration(2 * vsync_timing_->vsync_interval().get()));
  ScheduleUpdate(kSessionId2,
                 zx::time(0) + zx::duration(2 * vsync_timing_->vsync_interval().get()));

  EXPECT_TRUE(last_latched_times_.empty());

  RunLoopFor(vsync_timing_->vsync_interval());
  const zx::time kPresentationTime1 = Now();
  FireFramePresentedCallback();
  RunLoopUntilIdle();
  {
    // The first batch of updates should have been presented.
    auto result_map = last_latched_times_;
    EXPECT_EQ(last_presented_time_, kPresentationTime1);
    EXPECT_EQ(result_map.size(), 2u);  // Both sessions should have updates.
    EXPECT_EQ(result_map.at(kSessionId1).size(), 3u);
    EXPECT_EQ(result_map.at(kSessionId2).size(), 1u);
    for (auto& [session_id, present_map] : result_map) {
      for (auto& [present_id, latched_time] : present_map) {
        // We don't know latched time, but it should have been set.
        EXPECT_NE(latched_time, zx::time(0));
      }
    }
  }

  // End next frame.
  RunLoopFor(zx::sec(2));
  const zx::time kPresentationTime2 = Now();
  FireFramePresentedCallback();
  RunLoopUntilIdle();
  {
    // The second batch of updates should have been presented.
    auto result_map = last_latched_times_;
    EXPECT_EQ(last_presented_time_, kPresentationTime2);
    EXPECT_EQ(result_map.size(), 2u);
    EXPECT_EQ(result_map.at(kSessionId1).size(), 1u);
    EXPECT_EQ(result_map.at(kSessionId2).size(), 1u);
    for (auto& [session_id, present_map] : result_map) {
      for (auto& [present_id, latched_time] : present_map) {
        EXPECT_NE(latched_time, zx::time(0));
      }
    }
  }
}

TEST_F(FrameSchedulerTest, ReleaseFences_ShouldBeReceivedWhenTheNextPresentIsRendered) {
  constexpr SessionId kSession = 1;

  std::vector<zx::event> fences1 = utils::CreateEventArray(1);
  std::vector<zx_koid_t> fence1_koid = utils::ExtractKoids(fences1);
  std::vector<zx::event> fences2 = utils::CreateEventArray(1);
  std::vector<zx_koid_t> fence2_koid = utils::ExtractKoids(fences2);

  ScheduleUpdate(kSession, zx::time(0), std::move(fences1), /*squashable=*/false);
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_TRUE(last_received_fences_.empty());
  FireFramePresentedCallback();

  ScheduleUpdate(kSession, zx::time(0), std::move(fences2), /*squashable=*/false);
  RunLoopFor(zx::sec(1));
  EXPECT_THAT(utils::ExtractKoids(last_received_fences_), testing::ElementsAreArray(fence1_koid));
  FireFramePresentedCallback();

  ScheduleUpdate(kSession, zx::time(0), {}, /*squashable=*/false);
  RunLoopFor(zx::sec(1));
  EXPECT_THAT(utils::ExtractKoids(last_received_fences_), testing::ElementsAreArray(fence2_koid));
}

TEST_F(FrameSchedulerTest, SquashedPresents_ShouldHaveTheirFencesCombined) {
  constexpr SessionId kSessionId = 1;

  // Create release fences
  std::vector<zx::event> fences = utils::CreateEventArray(3);
  std::vector<zx_koid_t> fence_koids = utils::ExtractKoids(fences);
  fence_koids.pop_back();  // The last fences should not be sent out yet.

  // Schedule three presents, the first two should be squashed into the third.
  {
    std::vector<zx::event> fence1;
    fence1.push_back(std::move(fences[0]));
    ScheduleUpdate(kSessionId, Now() + vsync_timing_->vsync_interval(), std::move(fence1),
                   /*squashable=*/true);
  }

  {
    std::vector<zx::event> fence2;
    fence2.push_back(std::move(fences[1]));
    ScheduleUpdate(kSessionId, zx::time(0), std::move(fence2), /*squashable=*/true);
  }

  {
    std::vector<zx::event> fence3;
    fence3.push_back(std::move(fences[2]));
    ScheduleUpdate(kSessionId, zx::time(0), std::move(fence3), /*squashable=*/true);
  }

  // After 1 second, we've latched on all three updates. The fences for the first two should
  // therefore have been sent.
  RunLoopFor(zx::sec(1));
  EXPECT_EQ(update_sessions_call_count_, 1U);
  EXPECT_THAT(utils::ExtractKoids(last_received_fences_), testing::ElementsAreArray(fence_koids));
}

TEST_F(FrameSchedulerTest, SkippedPresents_ShouldHaveTheirFencesCombined) {
  constexpr SessionId kSessionId = 1;

  // Create release fences
  std::vector<zx::event> fences = utils::CreateEventArray(3);
  std::vector<zx_koid_t> fence_koids = utils::ExtractKoids(fences);
  fence_koids.pop_back();  // The last fences should not be sent out yet.

  // Register two presents but don't schedule them. Then when we actually schedule a third the first
  // two's fences should be sent on.
  {
    std::vector<zx::event> fence1;
    fence1.push_back(std::move(fences[0]));
    scheduler_.RegisterPresent(kSessionId, std::move(fence1));
  }
  {
    std::vector<zx::event> fence2;
    fence2.push_back(std::move(fences[1]));
    scheduler_.RegisterPresent(kSessionId, std::move(fence2));
  }
  {
    std::vector<zx::event> fence3;
    fence3.push_back(std::move(fences[2]));
    ScheduleUpdate(kSessionId, zx::time(0), std::move(fence3));
  }
  // After 1 second, we've latched on all three updates. The fences for the first two should
  // therefore have been sent.
  RunLoopFor(zx::sec(1));
  EXPECT_THAT(utils::ExtractKoids(last_received_fences_), testing::ElementsAreArray(fence_koids));
}

TEST_F(FrameSchedulerTest, DelayedRendering_ShouldProduceLatchedTimes) {
  constexpr SessionId kSessionId = 1;
  EXPECT_EQ(update_sessions_call_count_, 0u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // Schedule an update for now.
  zx::time now = Now();
  ScheduleUpdate(kSessionId, now);

  // Wait for one vsync period.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  EXPECT_EQ(update_sessions_call_count_, 1u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // Schedule 2 other updates for now, while Scenic is still rendering.
  ScheduleUpdate(kSessionId, now);
  ScheduleUpdate(kSessionId, now);
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  // Updates should be applied, but not rendered.
  EXPECT_EQ(update_sessions_call_count_, 2u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // Schedule 2 other updates for now, again while Scenic is still rendering.
  ScheduleUpdate(kSessionId, now);
  ScheduleUpdate(kSessionId, now);
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  // Updates should be applied, but not rendered.
  EXPECT_EQ(update_sessions_call_count_, 3u);
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // End previous frame.
  FireFramePresentedCallback();
  EXPECT_FALSE(frame_presented_callback_.has_value());
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  // We expect 1 latched time submitted in the first frame.
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 1u);

  // Second render should have occurred.
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // End second frame.
  FireFramePresentedCallback();
  EXPECT_FALSE(frame_presented_callback_.has_value());
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));

  // We expect 4 latched times submitted in the second frame.
  EXPECT_EQ(last_latched_times_.at(kSessionId).size(), 4u);
}

TEST_F(FrameSchedulerTest, RenderContinuously_ShouldCauseRenders_WithoutScheduledUpdates) {
  // No scheduled update. Run a vsync interval and observe no attempted renders.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_FALSE(frame_presented_callback_.has_value());

  scheduler_.SetRenderContinuously(true);

  // Still no scheduled updates. Run a vsync interval and observe an attempted render.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // With a frame pending we should see no more attempted renders until it is completed.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_TRUE(frame_presented_callback_.has_value());
  EXPECT_EQ(on_frame_presented_call_count_, 0u);

  FireFramePresentedCallback();
  EXPECT_EQ(on_frame_presented_call_count_, 1u);
  EXPECT_FALSE(frame_presented_callback_.has_value());

  // With the previous frame complete, we should now see another attempted render in the next vsync
  // interval.
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_TRUE(frame_presented_callback_.has_value());

  // After disabling continuous rendering we should no longer see attempted renders.
  scheduler_.SetRenderContinuously(false);
  FireFramePresentedCallback();
  RunLoopFor(zx::duration(vsync_timing_->vsync_interval()));
  EXPECT_FALSE(frame_presented_callback_.has_value());
}

}  // namespace scheduling::test
