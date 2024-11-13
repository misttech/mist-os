// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <bind/fuchsia/test/cpp/bind.h>
#include <gtest/gtest.h>
namespace testing {

class ParentDriverTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override { return zx::ok(); }
};

class TestConfig final {
 public:
  using DriverType = fdf_testing::EmptyDriverType;
  using EnvironmentType = ParentDriverTestEnvironment;
};

class ParentDriverTest : public ::testing::Test {
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

TEST_F(ParentDriverTest, VerifyChildNode) {
  // Access the driver's bound node and check that it's parenting one child node that has the
  // test property properly set to the name we gave it in the i2c name response.
  driver_test.RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    auto child = node.children().find("zircon_transport_rust_child");
    EXPECT_NE(child, node.children().end());
  });
}

TEST_F(ParentDriverTest, ConnectAndGetName) {
  zx::result connect_result = driver_test.Connect<fuchsia_hardware_i2c::Service::Device>();
  ASSERT_TRUE(connect_result.is_ok());

  fidl::WireSyncClient<fuchsia_hardware_i2c::Device> client(std::move(connect_result.value()));
  auto result = client->GetName();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());
  ASSERT_STREQ((*result)->name.cbegin(), "rust i2c server");
}

}  // namespace testing
