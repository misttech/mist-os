// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/lib/display/get_hardware_display_controller.h"

#include <lib/fpromise/promise.h>
#include <lib/fpromise/single_threaded_executor.h>
#include <lib/sys/cpp/component_context.h>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/lib/display/hardware_display_controller_provider_impl.h"

namespace ui_display {
namespace test {

struct fake_context : fpromise::context {
  fpromise::executor* executor() const override { return nullptr; }
  fpromise::suspended_task suspend_task() override { return fpromise::suspended_task(); }
};

class GetHardwareDisplayCoordinatorTest : public gtest::RealLoopFixture {};

TEST_F(GetHardwareDisplayCoordinatorTest, ErrorCase) {
  auto promise = GetHardwareDisplayCoordinator();
  fake_context context;
  EXPECT_TRUE(promise(context).is_error());
}

TEST_F(GetHardwareDisplayCoordinatorTest, WithHardwareDisplayCoordinatorProviderImpl) {
  std::unique_ptr<sys::ComponentContext> app_context = sys::ComponentContext::Create();
  ui_display::HardwareDisplayCoordinatorProviderImpl hdcp_service_impl(app_context.get());
  auto promise = GetHardwareDisplayCoordinator(&hdcp_service_impl);
  fake_context context;
  EXPECT_FALSE(promise(context).is_error());
}

}  // namespace test
}  // namespace ui_display
