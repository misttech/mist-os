// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.common/cpp/fidl.h>
#include <fidl/fuchsia.wlan.sme/cpp/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/driver/fidl.h>
#include <lib/driver/testing/cpp/fixture/driver_test_fixture.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fidl/cpp/client.h>

#include <gtest/gtest.h>
#include <src/connectivity/wlan/drivers/wlansoftmac/softmac_driver.h>

#include "custom_environment.h"
#include "fake_wlansoftmac_server.h"

namespace wlan::drivers::wlansoftmac {
namespace {

class FixtureConfig final {
 public:
  using DriverType = SoftmacDriver;
  using EnvironmentType = CustomEnvironment<BasicWlanSoftmacServer>;
};

class GoodSoftmacDriverTest : public fdf_testing::ForegroundDriverTestFixture<FixtureConfig>,
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

// Verify a clean startup and shutdown when wlansoftmac does not encounter any errors
// while running.
TEST_F(GoodSoftmacDriverTest, CleanStartupAndShutdown) {}

// Verify wlansoftmac creates a child node for an ethernet driver.
TEST_F(GoodSoftmacDriverTest, VerifyChildNode) {
  RunInNodeContext([](fdf_testing::TestNode& node) {
    ASSERT_EQ(1u, node.children().size());
    ASSERT_EQ(node.children().count("wlansoftmac-ethernet"), 1ul);

    auto expected_property = fdf::MakeProperty(1, ZX_PROTOCOL_ETHERNET_IMPL);
    auto properties = node.children().find("wlansoftmac-ethernet")->second.GetProperties();

    ASSERT_EQ(properties.size(), 1ul);
    ASSERT_EQ(properties[0], expected_property);
  });
}

}  // namespace
}  // namespace wlan::drivers::wlansoftmac
