// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

#include "examples/drivers/simple/dfv2/simple_driver.h"

namespace testing {

class SimpleDriverTestEnvironment {
 public:
  static std::unique_ptr<SimpleDriverTestEnvironment> CreateAndInitialize(
      fdf::OutgoingDirectory& to_driver_vfs) {
    // Perform any additional initialization here, such as setting up compat device servers
    // and FIDL servers.
    return std::make_unique<SimpleDriverTestEnvironment>();
  }
};

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = simple::SimpleDriver;
  using EnvironmentType = SimpleDriverTestEnvironment;
};

class SimpleDriverTest : public fdf_testing::DriverTestFixture<FixtureConfig> {};

TEST_F(SimpleDriverTest, VerifyChildNode) {
  RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    EXPECT_TRUE(node.children().count("simple_child"));
  });
}

}  // namespace testing
