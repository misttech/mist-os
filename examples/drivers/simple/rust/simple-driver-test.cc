// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/driver_test.h>

#include <bind/fuchsia/test/cpp/bind.h>
#include <gtest/gtest.h>
namespace testing {

// This example demonstrates a unit test for the simple rust driver wrapped by BackgroundDriverTest,
// which executes the simple driver on a background driver dispatcher and allows tests to use
// sync FIDL clients directly from the main test thread. It's recommended to use
// BackgroundDriverTest for rust drivers because you can only communicate with them through FIDL,
// and it's easier to do that through sync fidl calls.

// This class that provides custom environment configurations for the driver running
// under the test. Typically this is used to set up services that the driver interacts
// with.
class SimpleDriverTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    // Perform any additional initialization here, such as setting up compat device servers
    // and FIDL servers.
    return zx::ok();
  }
};

// Configuration class that provides information for BackgroundDriverTest. The class
// must match the following format and provide the driver class and the environment
// class.
class TestConfig final {
 public:
  // For a rust driver, you will always use fdf_testing::EmptyDriverType since you will not
  // be able to directly access the driver object from C++.
  using DriverType = fdf_testing::EmptyDriverType;
  using EnvironmentType = SimpleDriverTestEnvironment;
};

class SimpleDriverBackgroundTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test.StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void TearDown() override {
    zx::result<> result = driver_test.StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

 protected:
  fdf_testing::BackgroundDriverTest<TestConfig> driver_test;
};

TEST_F(SimpleDriverBackgroundTest, VerifyChildNode) {
  // Access the driver's bound node and check that it's parenting one child node.
  driver_test.RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    auto child = node.children().find("simple_child");
    EXPECT_NE(child, node.children().end());
    auto props = child->second.GetProperties();
    EXPECT_EQ(1u, props.size());
    auto prop = props.begin();
    EXPECT_STREQ(bind_fuchsia_test::TEST_CHILD.c_str(), prop->key().string_value()->c_str());
    EXPECT_STREQ("simple", prop->value().string_value()->c_str());
  });
}

}  // namespace testing
