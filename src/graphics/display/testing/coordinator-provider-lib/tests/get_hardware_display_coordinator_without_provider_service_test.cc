// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/executor.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/single_threaded_executor.h>
#include <lib/sys/cpp/component_context.h>

#include <gtest/gtest.h>

#include "src/graphics/display/testing/coordinator-provider-lib/client-hlcpp.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace display {

namespace {

class GetHardwareDisplayCoordinatorWithoutProviderServiceTest : public gtest::RealLoopFixture {
 protected:
  async::Executor& executor() { return executor_; }

 private:
  async::Executor executor_{dispatcher()};
};

TEST_F(GetHardwareDisplayCoordinatorWithoutProviderServiceTest, FailedOnNoProviderService) {
  std::optional<fpromise::result<CoordinatorClientEnd, zx_status_t>> coordinator;
  executor().schedule_task(GetCoordinator().then(
      [&coordinator](fpromise::result<CoordinatorClientEnd, zx_status_t>& result) {
        coordinator = std::move(result);
      }));
  RunLoopUntil([&coordinator] { return coordinator.has_value(); });
  ASSERT_TRUE(coordinator.value().is_error());
  EXPECT_EQ(coordinator.value().error(), ZX_ERR_NOT_FOUND);
}

}  // namespace

}  // namespace display
