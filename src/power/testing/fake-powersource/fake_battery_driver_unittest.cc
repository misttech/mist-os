// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.powersource.test/cpp/markers.h>
#include <fidl/fuchsia.hardware.powersource/cpp/wire.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <gtest/gtest.h>

#include "driver.h"
#include "src/lib/testing/predicates/status.h"

namespace fake_powersource::testing {

using fuchsia_hardware_powersource::wire::BatteryInfo;
using fuchsia_hardware_powersource::wire::BatteryUnit;
using fuchsia_hardware_powersource::wire::PowerType;

class FakeBatteryDriverTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override { return zx::ok(); }
};

class FixtureConfig final {
 public:
  using DriverType = Driver;
  using EnvironmentType = FakeBatteryDriverTestEnvironment;
};

class FakeBatteryDriverTest : public ::testing::Test {
 public:
  void SetUp() override {
    ASSERT_OK(driver_test_.StartDriver());

    zx::result fake_battery =
        driver_test_.ConnectThroughDevfs<fuchsia_hardware_powersource::Source>("fake-battery");
    ASSERT_OK(fake_battery);
    fake_battery_.Bind(std::move(fake_battery.value()));

    zx::result battery_source_simulator =
        driver_test_.ConnectThroughDevfs<fuchsia_hardware_powersource_test::SourceSimulator>(
            "battery-source-simulator");
    ASSERT_OK(battery_source_simulator);
    battery_source_simulator_.Bind(std::move(battery_source_simulator.value()));
  }

  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

 protected:
  fidl::WireSyncClient<fuchsia_hardware_powersource::Source>& fake_battery() {
    return fake_battery_;
  }
  fidl::WireSyncClient<fuchsia_hardware_powersource_test::SourceSimulator>&
  battery_source_simulator() {
    return battery_source_simulator_;
  }

 private:
  fidl::WireSyncClient<fuchsia_hardware_powersource::Source> fake_battery_;
  fidl::WireSyncClient<fuchsia_hardware_powersource_test::SourceSimulator>
      battery_source_simulator_;
  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;
};

TEST_F(FakeBatteryDriverTest, CanGetInfo) {
  {
    fidl::WireResult result = fake_battery()->GetPowerInfo();
    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result.value().status, ZX_OK);
    const auto& info = result.value().info;
    ASSERT_EQ(info.type, PowerType::kBattery);
    ASSERT_EQ(info.state, fuchsia_hardware_powersource::kPowerStateCharging |
                              fuchsia_hardware_powersource::kPowerStateOnline);
  }
  {
    fidl::WireResult result = fake_battery()->GetBatteryInfo();
    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result.value().status, ZX_OK);
    const auto& info = result.value().info;
    ASSERT_EQ(info.present_rate, 2);
  }
}

TEST_F(FakeBatteryDriverTest, CatGetEvent) {
  {
    fidl::WireResult result = fake_battery()->GetBatteryInfo();
    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result.value().status, ZX_OK);
    const auto& info = result.value().info;
    ASSERT_EQ(info.remaining_capacity, 2950u);
  }
  {
    fidl::WireResult result = fake_battery()->GetStateChangeEvent();
    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result.value().status, ZX_OK);
    auto event = std::move(result.value().handle);
    zx_signals_t pending = 0;
    ASSERT_EQ(event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite_past(), &pending),
              ZX_ERR_TIMED_OUT);
    ASSERT_EQ(pending, 0u);

    BatteryInfo battery_info{
        .unit = BatteryUnit::kMa,
        .design_capacity = 3000,
        .last_full_capacity = 2950,
        .design_voltage = 3000,  // mV
        .capacity_warning = 800,
        .capacity_low = 500,
        .capacity_granularity_low_warning = 20,
        .capacity_granularity_warning_full = 1,
        .present_rate = 2,
        .remaining_capacity = 45,  // Only changed this value
        .present_voltage = 2900,
    };
    auto result2 = battery_source_simulator()->SetBatteryInfo(battery_info);
    ASSERT_EQ(result2.status(), ZX_OK);

    ASSERT_EQ(event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), &pending), ZX_OK);
    ASSERT_EQ(pending, ZX_USER_SIGNAL_0);
  }
  {
    fidl::WireResult result = fake_battery()->GetBatteryInfo();
    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result.value().status, ZX_OK);
    const auto& info = result.value().info;
    ASSERT_EQ(info.remaining_capacity, 45u);
  }
}

}  // namespace fake_powersource::testing
