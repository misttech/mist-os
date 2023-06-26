// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/loop.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/single_threaded_executor.h>

#include <gtest/gtest.h>

#include "src/graphics/display/testing/coordinator-provider-lib/client-hlcpp.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace display {

namespace {

// Tests the code path when the service routing is available.
class GetHardwareDisplayCoordinatorWithProviderServiceTest : public gtest::RealLoopFixture {};

// FIDL and Async executor should be able to run on a single dispatcher.
TEST_F(GetHardwareDisplayCoordinatorWithProviderServiceTest, SingleDispatcher) {
  std::optional<fpromise::result<CoordinatorClientEnd, zx_status_t>> coordinator;
  async::Executor executor(dispatcher());

  executor.schedule_task(
      GetCoordinator(dispatcher())
          .then([&coordinator](fpromise::result<CoordinatorClientEnd, zx_status_t>& result) {
            coordinator = std::move(result);
          }));

  // After the service is opened, an fuchsia.io.Node.OnOpen() event will be
  // dispatched to the FIDL dispatcher, before which the loop will be idle.
  // Tests should use RunLoopUntil() to wait until the coordinator is fetched.
  RunLoopUntil([&] { return coordinator.has_value(); });
  ASSERT_TRUE(coordinator.value().is_ok()) << "Failed to get coordinator client end: "
                                           << zx_status_get_string(coordinator.value().error());
  EXPECT_TRUE(coordinator.value().value().is_valid());
}

// FIDL and Async executor should be able to run on separate dispatchers.
TEST_F(GetHardwareDisplayCoordinatorWithProviderServiceTest, SeparateDispatchers) {
  async::Loop fidl_loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl_loop.StartThread("fidl-loop");

  std::optional<fpromise::result<CoordinatorClientEnd, zx_status_t>> coordinator;
  async::Executor executor(dispatcher());

  executor.schedule_task(
      GetCoordinator(fidl_loop.dispatcher())
          .then([&coordinator](fpromise::result<CoordinatorClientEnd, zx_status_t>& result) {
            coordinator = std::move(result);
          }));

  RunLoopUntil([&] { return coordinator.has_value(); });
  ASSERT_TRUE(coordinator.value().is_ok()) << "Failed to get coordinator client end: "
                                           << zx_status_get_string(coordinator.value().error());
  EXPECT_TRUE(coordinator.value().value().is_valid());

  fidl_loop.Shutdown();
}

}  // namespace

}  // namespace display
