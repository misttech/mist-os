// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>

#include "src/performance/trace_manager/tests/trace_manager_test.h"

namespace tracing {
namespace test {

using SessionState = TraceManagerTest::SessionState;

// This only works when no other condition could cause the loop to exit.
// E.g., This doesn't work if the state is kStopping or kTerminating as the
// transition to kStopped,kTerminated will also cause the loop to exit.
template <typename T>
controller::Session_StartTracing_Result TryStart(TraceManagerTest* fixture,
                                                 const T& interface_ptr) {
  controller::Session_StartTracing_Result start_result;
  controller::StartOptions start_options{fixture->GetDefaultStartOptions()};
  bool start_completed = false;
  interface_ptr->StartTracing(
      std::move(start_options),
      [fixture, &start_completed, &start_result](controller::Session_StartTracing_Result result) {
        start_completed = true;
        start_result = std::move(result);
        fixture->QuitLoop();
      });
  fixture->RunLoopUntilIdle();
  FX_LOGS(DEBUG) << "Start loop done";
  EXPECT_TRUE(start_completed);
  return start_result;
}

template <typename T>
void TryExtraStart(TraceManagerTest* fixture, const T& interface_ptr) {
  controller::Session_StartTracing_Result start_result{TryStart(fixture, interface_ptr)};
  EXPECT_EQ(fixture->GetSessionState(), SessionState::kStarted);
  EXPECT_TRUE(start_result.is_err());
  EXPECT_EQ(start_result.err(), controller::StartError::ALREADY_STARTED);
}

TEST_F(TraceManagerTest, ExtraStart) {
  ConnectToProvisionerService();

  EXPECT_TRUE(AddFakeProvider(kProvider1Pid, kProvider1Name));

  ASSERT_TRUE(InitializeSession());

  ASSERT_TRUE(StartSession());

  // Now try starting again.
  TryExtraStart(this, controller());
}

TEST_F(TraceManagerTest, StartWhileStopping) {
  ConnectToProvisionerService();

  EXPECT_TRUE(AddFakeProvider(kProvider1Pid, kProvider1Name));

  ASSERT_TRUE(InitializeSession());

  ASSERT_TRUE(StartSession());

  controller::StopOptions stop_options{GetDefaultStopOptions()};
  controller()->StopTracing(
      std::move(stop_options),
      [](controller::Session_StopTracing_Result result) { ASSERT_TRUE(result.is_response()); });
  RunLoopUntilIdle();
  // The loop will exit for the transition to kStopping.
  FX_LOGS(DEBUG) << "Loop done, expecting session stopping";
  ASSERT_EQ(GetSessionState(), SessionState::kStopping);

  // Now try a Start while we're still in |kStopping|.
  // The provider doesn't advance state until we tell it to, so we should
  // still remain in |kStopping|.
  controller::Session_StartTracing_Result result;
  controller::StartOptions start_options{GetDefaultStartOptions()};
  bool start_completed = false;
  controller()->StartTracing(
      std::move(start_options),
      [this, &start_completed, &result](controller::Session_StartTracing_Result in_result) {
        start_completed = true;
        result = std::move(in_result);
        QuitLoop();
      });
  RunLoopUntilIdle();
  FX_LOGS(DEBUG) << "Start loop done";
  EXPECT_TRUE(GetSessionState() == SessionState::kStopping);
  ASSERT_TRUE(start_completed);
  ASSERT_TRUE(result.is_err());
  EXPECT_EQ(result.err(), controller::StartError::STOPPING);
}

}  // namespace test
}  // namespace tracing
