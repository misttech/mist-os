// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>

#include "examples/drivers/template/template_driver.h"

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
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = template_driver::TemplateDriver;
  using EnvironmentType = SimpleDriverTestEnvironment;
};

class SimpleDriverTest : public fdf_testing::DriverTestFixture<FixtureConfig> {};

TEST_F(SimpleDriverTest, VerifyChildNode) {
  RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    EXPECT_TRUE(node.children().count("example_child"));
  });
}

}  // namespace testing
