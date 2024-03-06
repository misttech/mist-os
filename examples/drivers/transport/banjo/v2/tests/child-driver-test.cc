// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/banjo/v2/child-driver.h"

#include <lib/async-loop/loop.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>

#include <ddktl/device.h>
#include <gtest/gtest.h>

namespace testing {

namespace {
constexpr uint32_t kTestHardwareId = 0x1234567;
constexpr uint32_t kTestMajorVersion = 0x9;
constexpr uint32_t kTestMinorVersion = 0x5;
}  // namespace

// A fake banjo server serving the Misc protocol.
class FakeParentBanjoServer : public ddk::MiscProtocol<FakeParentBanjoServer, ddk::base_protocol> {
 public:
  zx_status_t MiscGetHardwareId(uint32_t* out_response) {
    *out_response = kTestHardwareId;
    return ZX_OK;
  }

  zx_status_t MiscGetFirmwareVersion(uint32_t* out_major, uint32_t* out_minor) {
    *out_major = kTestMajorVersion;
    *out_minor = kTestMinorVersion;
    return ZX_OK;
  }

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_MISC};
    config.callbacks[ZX_PROTOCOL_MISC] = banjo_server_.callback();
    return config;
  }

 private:
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_MISC, this, &misc_protocol_ops_};
};

class TestEnvironmentWrapper {
 public:
  fdf::DriverStartArgs Init() {
    zx::result start_args_result = node_.CreateStartArgsAndServe();
    ZX_ASSERT(start_args_result.is_ok());

    zx::result init_result =
        test_environment_.Initialize(std::move(start_args_result->incoming_directory_server));
    ZX_ASSERT(init_result.is_ok());

    device_server_.Init("default", "", std::nullopt, banjo_server_.GetBanjoConfig());
    ZX_ASSERT(device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   &test_environment_.incoming_directory()) == ZX_OK);
    return std::move(start_args_result->start_args);
  }

 private:
  fdf_testing::TestNode node_{"root"};
  fdf_testing::TestEnvironment test_environment_;

  compat::DeviceServer device_server_;
  FakeParentBanjoServer banjo_server_;
};

class ChildBanjoTransportDriverTest : public testing::Test {
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

  banjo_transport::ChildBanjoTransportDriver* driver() { return *driver_; }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher test_env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<TestEnvironmentWrapper> environment_{env_dispatcher(),
                                                                           std::in_place};

  fdf_testing::DriverUnderTest<banjo_transport::ChildBanjoTransportDriver> driver_;
};

TEST_F(ChildBanjoTransportDriverTest, VerifyQueryValues) {
  // Verify that the queried values match the fake banjo server.
  EXPECT_EQ(kTestHardwareId, driver()->hardware_id());
  EXPECT_EQ(kTestMajorVersion, driver()->major_version());
  EXPECT_EQ(kTestMinorVersion, driver()->minor_version());
}

}  // namespace testing
