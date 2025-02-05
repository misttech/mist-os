// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_power_manager.h"

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fuchsia/ui/display/internal/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/async/time.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/reader.h>

#include <cstdint>
#include <thread>
#include <unordered_set>

#include <gtest/gtest.h>

#include "lib/inspect/cpp/hierarchy.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"

namespace scenic_impl::gfx::test {

namespace {

struct DisplayPowerInfo {
  bool power_on = true;
  int64_t timestamp;
};

class DisplayPowerManagerMockTest : public gtest::RealLoopFixture {
 public:
  DisplayPowerManagerMockTest() {
    display_manager_ = std::make_unique<display::DisplayManager>([] {});
    display_power_manager_ =
        std::make_unique<display::DisplayPowerManager>(*display_manager_, inspector_.GetRoot());
  }

  display::DisplayManager* display_manager() { return display_manager_.get(); }
  display::DisplayPowerManager* display_power_manager() { return display_power_manager_.get(); }
  display::Display* display() { return display_manager()->default_display(); }
  DisplayPowerInfo GetLastDisplayPowerInspectValue() {
    auto result = inspect::ReadFromVmo(inspector_.DuplicateVmo());
    EXPECT_TRUE(result.is_ok());
    auto hierarchy = result.take_value();
    const auto& power_node = hierarchy.children()[0];
    EXPECT_EQ("display_power_events", power_node.name());

    DisplayPowerInfo info;
    if (power_node.children().empty()) {
      return info;
    }
    const auto& property = power_node.children().back().node().properties()[0];
    const auto& name = property.name();
    EXPECT_TRUE(name == "on" || name == "off");
    info.power_on = name == "on";
    info.timestamp = property.Get<inspect::IntPropertyValue>().value();
    EXPECT_NE(info.timestamp, 0);
    return info;
  }

 private:
  inspect::Inspector inspector_;
  std::unique_ptr<display::DisplayManager> display_manager_;
  std::unique_ptr<display::DisplayPowerManager> display_power_manager_;
};

TEST_F(DisplayPowerManagerMockTest, Ok) {
  const fuchsia_hardware_display_types::DisplayId kDisplayId = {{.value = 1}};
  const uint32_t kDisplayWidth = 1024;
  const uint32_t kDisplayHeight = 768;

  auto [coordinator_client, coordinator_server] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [listener_client, listener_server] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_client),
                                                   std::move(listener_server));

  display_manager()->SetDefaultDisplayForTests(
      std::make_shared<display::Display>(kDisplayId, kDisplayWidth, kDisplayHeight));

  display::test::MockDisplayCoordinator mock_display_coordinator(fuchsia_hardware_display::Info{});
  mock_display_coordinator.Bind(std::move(coordinator_server), std::move(listener_client),
                                dispatcher());
  mock_display_coordinator.set_set_display_power_result(ZX_OK);

  RunLoopUntilIdle();
  auto power_info_1 = GetLastDisplayPowerInspectValue();
  EXPECT_TRUE(power_info_1.power_on);
  auto last_power_timestamp = power_info_1.timestamp;
  {
    bool callback_executed = false;
    std::thread set_display_power_thread([&callback_executed, this] {
      display_power_manager()->SetDisplayPower(
          /* power_on */ false, [&callback_executed](fit::result<zx_status_t> result) {
            callback_executed = true;
            EXPECT_TRUE(result.is_ok());
          });
    });

    RunLoopUntil([&callback_executed] { return callback_executed; });
    auto power_info_2 = GetLastDisplayPowerInspectValue();
    EXPECT_FALSE(power_info_2.power_on);
    EXPECT_GT(power_info_2.timestamp, last_power_timestamp);
    last_power_timestamp = power_info_2.timestamp;
    set_display_power_thread.join();
    EXPECT_FALSE(mock_display_coordinator.display_power_on());
  }

  {
    bool callback_executed = false;
    std::thread set_display_power_thread([&callback_executed, this] {
      display_power_manager()->SetDisplayPower(
          /* power_on */ true, [&callback_executed](fit::result<zx_status_t> result) {
            callback_executed = true;
            EXPECT_TRUE(result.is_ok());
          });
    });

    RunLoopUntil([&callback_executed] { return callback_executed; });
    auto power_info_3 = GetLastDisplayPowerInspectValue();
    EXPECT_TRUE(power_info_3.power_on);
    EXPECT_GT(power_info_3.timestamp, last_power_timestamp);
    set_display_power_thread.join();
    EXPECT_TRUE(mock_display_coordinator.display_power_on());
  }
}

TEST_F(DisplayPowerManagerMockTest, NoDisplay) {
  auto [coordinator_client, coordinator_server] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [listener_client, listener_server] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_client),
                                                   std::move(listener_server));

  display_manager()->SetDefaultDisplayForTests(nullptr);

  display::test::MockDisplayCoordinator mock_display_coordinator(fuchsia_hardware_display::Info{});
  mock_display_coordinator.Bind(std::move(coordinator_server), std::move(listener_client),
                                dispatcher());

  RunLoopUntilIdle();

  {
    bool callback_executed = false;
    std::thread set_display_power_thread([&callback_executed, this] {
      display_power_manager()->SetDisplayPower(
          /* power_on */ false, [&callback_executed](fit::result<zx_status_t> result) {
            callback_executed = true;
            ASSERT_TRUE(result.is_error());
            EXPECT_EQ(result.error_value(), ZX_ERR_NOT_FOUND);
          });
    });

    RunLoopUntil([&callback_executed] { return callback_executed; });
    set_display_power_thread.join();
  }
}

TEST_F(DisplayPowerManagerMockTest, NotSupported) {
  const fuchsia_hardware_display_types::DisplayId kDisplayId = {{.value = 1}};
  const uint32_t kDisplayWidth = 1024;
  const uint32_t kDisplayHeight = 768;

  auto [coordinator_client, coordinator_server] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [listener_client, listener_server] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_client),
                                                   std::move(listener_server));

  display_manager()->SetDefaultDisplayForTests(
      std::make_shared<display::Display>(kDisplayId, kDisplayWidth, kDisplayHeight));

  display::test::MockDisplayCoordinator mock_display_coordinator(fuchsia_hardware_display::Info{});
  mock_display_coordinator.Bind(std::move(coordinator_server), std::move(listener_client),
                                dispatcher());
  mock_display_coordinator.set_set_display_power_result(ZX_ERR_NOT_SUPPORTED);

  RunLoopUntilIdle();

  {
    bool callback_executed = false;
    std::thread set_display_power_thread([&callback_executed, this] {
      display_power_manager()->SetDisplayPower(
          /* power_on */ false, [&callback_executed](fit::result<zx_status_t> result) {
            callback_executed = true;
            EXPECT_TRUE(result.is_error());
            EXPECT_EQ(result.error_value(), ZX_ERR_NOT_SUPPORTED);
          });
    });

    RunLoopUntil([&callback_executed] { return callback_executed; });
    set_display_power_thread.join();
  }
}

}  // namespace

}  // namespace scenic_impl::gfx::test
