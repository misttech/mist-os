// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/driver/v2/child-driver.h"

#include <lib/driver/testing/cpp/fixture/driver_test_fixture.h>

#include <gtest/gtest.h>

namespace testing {

namespace {
constexpr uint32_t kTestMaxTransferSize = 0x1234567;
constexpr uint32_t kTestBitrate = 0x5;
}  // namespace

class FakeI2cImplServer : public fdf::WireServer<fuchsia_hardware_i2cimpl::Device> {
 public:
  void GetMaxTransferSize(fdf::Arena& arena,
                          GetMaxTransferSizeCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess(kTestMaxTransferSize);
  }

  void SetBitrate(SetBitrateRequestView request, fdf::Arena& arena,
                  SetBitrateCompleter::Sync& completer) override {
    bitrate = request->bitrate;
    completer.buffer(arena).ReplySuccess();
  }

  void Transact(TransactRequestView request, fdf::Arena& arena,
                TransactCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess({});
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_i2cimpl::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(
        ERROR,
        "Unknown method in fuchsia.hardware.i2cimpl Device protocol, closing with ZX_ERR_NOT_SUPPORTED");
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  uint32_t bitrate;
};

class DriverTransportTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    fuchsia_hardware_i2cimpl::Service::InstanceHandler handler({
        .device = server_bindings_.CreateHandler(&server_, fdf::Dispatcher::GetCurrent()->get(),
                                                 fidl::kIgnoreBindingClosure),
    });

    auto result = to_driver_vfs.AddService<fuchsia_hardware_i2cimpl::Service>(std::move(handler));
    EXPECT_EQ(ZX_OK, result.status_value());
    return zx::ok();
  }

  uint32_t GetBitrate() const { return server_.bitrate; }

 private:
  FakeI2cImplServer server_;
  fdf::ServerBindingGroup<fuchsia_hardware_i2cimpl::Device> server_bindings_;
};

class FixtureConfig final {
 public:
  using DriverType = driver_transport::ChildTransportDriver;
  using EnvironmentType = DriverTransportTestEnvironment;
};

class ChildTransportDriverTest : public fdf_testing::ForegroundDriverTestFixture<FixtureConfig>,
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

TEST_F(ChildTransportDriverTest, VerifyQueryValues) {
  // Verify that the queried values match the fake parent driver server.
  EXPECT_EQ(kTestMaxTransferSize, driver()->max_transfer_size());

  RunInEnvironmentTypeContext(
      [&](DriverTransportTestEnvironment& env) { EXPECT_EQ(kTestBitrate, env.GetBitrate()); });
}

}  // namespace testing
