// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/fixture/driver_test_fixture.h>

#include <gtest/gtest.h>

#include "examples/drivers/simple/dfv2/simple_driver.h"

namespace testing {

class SimpleDriverTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    // Perform any additional initialization here, such as setting up compat device servers
    // and FIDL servers.
    return zx::ok();
  }
};

class FixtureConfig final {
 public:
  using DriverType = simple::SimpleDriver;
  using EnvironmentType = SimpleDriverTestEnvironment;
};

class SimpleDriverTest : public fdf_testing::ForegroundDriverTestFixture<FixtureConfig>,
                         public ::testing::Test {
  void SetUp() override {
    zx::result<> result = StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void TearDown() override {
    zx::result<> result = StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
};

TEST_F(SimpleDriverTest, VerifyChildNode) {
  RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    EXPECT_TRUE(node.children().count("simple_child"));
  });
}

}  // namespace testing
