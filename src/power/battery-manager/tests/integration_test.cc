// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.power.battery/cpp/fidl.h>
#include <fidl/fuchsia.power.battery/cpp/wire_test_base.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

namespace battery_manager::testing {
namespace {

using fidl::testing::WireTestBase;

class FakeInfoWatcher : public WireTestBase<::fuchsia_power_battery::BatteryInfoWatcher> {
 public:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    FX_LOGS(ERROR) << "Called NotImplemented";
    FAIL();
  }
  void OnChangeBatteryInfo(OnChangeBatteryInfoRequestView req,
                           OnChangeBatteryInfoCompleter::Sync& completer) override {
    called_ = true;
    completer.Reply();
  }
  bool called() const { return called_; }

 private:
  bool called_ = false;
};

class BatteryIntegrationTest : public gtest::RealLoopFixture {
 public:
  void SetUp() override {
    RealLoopFixture::SetUp();

    // Initialize client_ for BatteryManager
    zx::result client_end = component::Connect<fuchsia_power_battery::BatteryManager>();
    ASSERT_TRUE(client_end.is_ok())
        << "Synchronous error when connecting to the |BatteryManager| protocol: "
        << client_end.status_string();

    client_.Bind(std::move(*client_end));
    ASSERT_TRUE(client_.is_valid());
  }

 protected:
  fidl::ClientEnd<fuchsia_power_battery::BatteryInfoWatcher> TakeWatcher() {
    return watcher_client_.TakeClientEnd();
  }

  void CreateAndConnectToWatcher() {
    watcher_ = std::make_unique<FakeInfoWatcher>();
    auto endpoints = fidl::CreateEndpoints<fuchsia_power_battery::BatteryInfoWatcher>();
    EXPECT_EQ(endpoints.status_value(), ZX_OK);
    fidl::BindServer(dispatcher(), std::move(endpoints->server), watcher_.get());
    watcher_client_.Bind(std::move(endpoints->client));
  }

  bool WatcherCalled() { return watcher_->called(); }

  fidl::SyncClient<fuchsia_power_battery::BatteryManager> client_;

 private:
  fidl::SyncClient<fuchsia_power_battery::BatteryInfoWatcher> watcher_client_;
  std::unique_ptr<FakeInfoWatcher> watcher_;
};

TEST_F(BatteryIntegrationTest, TestGetInfo) {
  fidl::Result result = client_->GetBatteryInfo();
  EXPECT_TRUE(result.is_ok()) << "GetBatteryInfo() failed: " << result.error_value();
}

// Call Watch, and expects the callback
TEST_F(BatteryIntegrationTest, TestWatch) {
  CreateAndConnectToWatcher();
  auto result = client_->Watch(TakeWatcher());
  ASSERT_TRUE(result.is_ok()) << "Watch() failed: " << result.error_value();

  FX_LOGS(INFO) << "Calling Watch(), waiting for the callback";
  RunLoopUntil([&] { return WatcherCalled(); });
}

}  // namespace
}  // namespace battery_manager::testing
