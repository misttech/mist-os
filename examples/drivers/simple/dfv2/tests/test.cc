// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/loop.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>

#include <gtest/gtest.h>

#include "examples/drivers/simple/dfv2/simple_driver.h"

namespace testing {

class TestEnvironmentWrapper {
 public:
  fdf::DriverStartArgs Init() {
    zx::result start_args_result = node_.CreateStartArgsAndServe();
    ZX_ASSERT(start_args_result.is_ok());

    zx::result init_result =
        test_environment_.Initialize(std::move(start_args_result->incoming_directory_server));
    ZX_ASSERT(init_result.is_ok());

    return std::move(start_args_result.value().start_args);
  }

  void VerifyChildNode() {
    EXPECT_EQ(1u, node_.children().size());
    EXPECT_TRUE(node_.children().count("simple_child"));
  }

 private:
  fdf_testing::TestNode node_{"root"};
  fdf_testing::TestEnvironment test_environment_;
};

class SimpleDriverTest : public testing::Test {
 public:
  void SetUp() override {
    fdf::DriverStartArgs start_args = environment_.SyncCall(&TestEnvironmentWrapper::Init);
    zx::result driver = runtime_.RunToCompletion(driver_.Start(std::move(start_args)));
    EXPECT_EQ(ZX_OK, driver.status_value());
  }

  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_->async_dispatcher(); }

  async_patterns::TestDispatcherBound<TestEnvironmentWrapper>& environment() {
    return environment_;
  }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher test_env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<TestEnvironmentWrapper> environment_{env_dispatcher(),
                                                                           std::in_place};

  fdf_testing::DriverUnderTest<simple::SimpleDriver> driver_;
};

TEST_F(SimpleDriverTest, VerifyChildNode) {
  environment().SyncCall(&TestEnvironmentWrapper::VerifyChildNode);
}

}  // namespace testing
