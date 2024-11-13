// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <bind/fuchsia/test/cpp/bind.h>
#include <gtest/gtest.h>
namespace testing {

namespace {

const std::string kTestName = "test_i2c";

}  // namespace

class FakeI2cServer : public fidl::WireServer<fuchsia_hardware_i2c::Device> {
 public:
  FakeI2cServer() {
    read_buffer_ = {0xA, 0xB, 0xC};
    read_vectors_.emplace_back(fidl::VectorView<uint8_t>::FromExternal(read_buffer_));
  }

  void Transfer(TransferRequestView request, TransferCompleter::Sync& completer) override {
    completer.ReplySuccess(
        fidl::VectorView<fidl::VectorView<uint8_t>>::FromExternal(read_vectors_));
  }

  void GetName(GetNameCompleter::Sync& completer) override {
    completer.ReplySuccess(fidl::StringView::FromExternal(kTestName));
  }

 private:
  std::vector<fidl::VectorView<uint8_t>> read_vectors_;
  std::vector<uint8_t> read_buffer_;
};

class ChildDriverTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    fuchsia_hardware_i2c::Service::InstanceHandler handler({
        .device =
            bindings_.CreateHandler(&server_, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                    fidl::kIgnoreBindingClosure),
    });
    auto result = to_driver_vfs.AddService<fuchsia_hardware_i2c::Service>(std::move(handler));
    EXPECT_EQ(ZX_OK, result.status_value());
    return zx::ok();
  }

 private:
  FakeI2cServer server_;
  fidl::ServerBindingGroup<fuchsia_hardware_i2c::Device> bindings_;
};

class TestConfig final {
 public:
  using DriverType = fdf_testing::EmptyDriverType;
  using EnvironmentType = ChildDriverTestEnvironment;
};

class ChildDriverTest : public ::testing::Test {
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

TEST_F(ChildDriverTest, VerifyChildNode) {
  // Access the driver's bound node and check that it's parenting one child node that has the
  // test property properly set to the name we gave it in the i2c name response.
  driver_test.RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    auto child = node.children().find("transport-child");
    EXPECT_NE(child, node.children().end());
    auto props = child->second.GetProperties();
    EXPECT_EQ(1u, props.size());
    auto prop = props.begin();
    EXPECT_EQ(bind_fuchsia_test::TEST_CHILD, prop->key().string_value().value());
    EXPECT_EQ(kTestName, prop->value().string_value().value());
  });
}

}  // namespace testing
