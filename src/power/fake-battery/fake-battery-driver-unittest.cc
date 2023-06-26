// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fake-battery-driver.h"

#include <lib/async-loop/loop.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>
#include <lib/syslog/cpp/macros.h>

#include <gtest/gtest.h>

#include "fidl/fuchsia.hardware.powersource/cpp/natural_types.h"

using fuchsia_hardware_powersource::wire::BatteryUnit;
using fuchsia_hardware_powersource::wire::PowerType;

// If the environment needs to run on a background driver dispatcher (for example if the driver
// needs to make sync FIDL calls), we need to run the environment on a background dispatcher while
// keeping the driver on the main thread.
class FakeBatteryDriverTest : public ::testing::Test {
 public:
  static void RunSyncClientTask(fit::closure task) {
    // Spawn a separate thread to run the client task using an async::Loop.
    async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
    loop.StartThread();
    zx::result result = fdf::RunOnDispatcherSync(loop.dispatcher(), std::move(task));
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());
    zx::result driver = runtime_.RunToCompletion(driver_.Start(std::move(start_args->start_args)));
    EXPECT_EQ(ZX_OK, driver.status_value());
  }

  void TearDown() override {
    zx::result result = runtime_.RunToCompletion(driver_.PrepareStop());
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  fdf_testing::DriverUnderTest<fake_battery::Driver>& driver() { return driver_; }

  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_->async_dispatcher(); }

  async_patterns::TestDispatcherBound<fdf_testing::TestNode>& node_server() { return node_server_; }

 private:
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher. Managed by driver runtime threads because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher test_env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  // The env_dispatcher is an fdf_dispatcher so we can add driver transport FIDL servers into this
  // environment.
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_dispatcher(), std::in_place};

  fdf_testing::DriverUnderTest<fake_battery::Driver> driver_;
};

TEST_F(FakeBatteryDriverTest, CanGetInfo) {
  // Safe to touch the driver from here since the driver_dispatcher is the default.
  // Dispatcher allows sync calls from the driver so we use the sync version.
  zx::result device_result = node_server().SyncCall([](fdf_testing::TestNode* root_node) {
    return root_node->children().at("fake-battery").ConnectToDevice();
  });
  ASSERT_EQ(ZX_OK, device_result.status_value());

  fidl::ClientEnd<fuchsia_hardware_powersource::Source> device_client_end(
      std::move(device_result.value()));

  fidl::WireSyncClient<fuchsia_hardware_powersource::Source> client(std::move(device_client_end));
  RunSyncClientTask([this_client = std::move(client)]() {
    {
      fidl::WireResult result = this_client->GetPowerInfo();
      ASSERT_EQ(result.status(), ZX_OK);
      ASSERT_EQ(result.value().status, ZX_OK);
      const auto& info = result.value().info;
      ASSERT_EQ(info.type, PowerType::kBattery);
      ASSERT_EQ(info.state, fuchsia_hardware_powersource::kPowerStateCharging |
                                fuchsia_hardware_powersource::kPowerStateOnline);
    }
    {
      fidl::WireResult result = this_client->GetBatteryInfo();
      ASSERT_EQ(result.status(), ZX_OK);
      ASSERT_EQ(result.value().status, ZX_OK);
      const auto& info = result.value().info;
      ASSERT_EQ(info.present_rate, 2);
    }
  });
}
