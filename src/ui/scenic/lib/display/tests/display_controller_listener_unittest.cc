// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_controller_listener.h"

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/images2/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>

#include <gtest/gtest.h>

#include "lib/fidl/cpp/comparison.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/ui/scenic/lib/display/tests/mock_display_controller.h"

namespace scenic_impl {
namespace display {
namespace test {

namespace {

struct ChannelPair {
  zx::channel server;
  zx::channel client;
};

ChannelPair CreateChannelPair() {
  ChannelPair c;
  FX_CHECK(ZX_OK == zx::channel::create(0, &c.server, &c.client));
  return c;
}

}  // namespace

class DisplayCoordinatorListenerTest : public gtest::TestLoopFixture {
 public:
  void SetUp() {
    ChannelPair coordinator_channel = CreateChannelPair();

    mock_display_coordinator_ = std::make_unique<MockDisplayCoordinator>();
    mock_display_coordinator_->Bind(std::move(coordinator_channel.server));

    auto coordinator = std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
    coordinator->Bind(std::move(coordinator_channel.client));
    display_coordinator_listener_ = std::make_unique<DisplayCoordinatorListener>(coordinator);
  }

  DisplayCoordinatorListener* display_coordinator_listener() {
    return display_coordinator_listener_.get();
  }

  void ResetMockDisplayCoordinator() { mock_display_coordinator_.reset(); }
  void ResetDisplayCoordinatorListener() { display_coordinator_listener_.reset(); }

  MockDisplayCoordinator* mock_display_coordinator() { return mock_display_coordinator_.get(); }

 private:
  std::unique_ptr<MockDisplayCoordinator> mock_display_coordinator_;
  std::unique_ptr<DisplayCoordinatorListener> display_coordinator_listener_;
};

using DisplayCoordinatorListenerBasicTest = gtest::TestLoopFixture;

// Verify the documented constructor behavior.
TEST_F(DisplayCoordinatorListenerBasicTest, ConstructorArgs) {
  {
    // Valid arguments.
    ChannelPair coordinator_channel = CreateChannelPair();

    auto coordinator = std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
    coordinator->Bind(std::move(coordinator_channel.client));
    DisplayCoordinatorListener listener(coordinator);

    EXPECT_TRUE(listener.valid());
  }

  {
    ChannelPair coordinator_channel = CreateChannelPair();

    // Unbound coordinator.
    auto coordinator = std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
    DisplayCoordinatorListener listener(coordinator);

    EXPECT_FALSE(listener.valid());
  }

  {
    ChannelPair device_channel = CreateChannelPair();

    auto coordinator = std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
    coordinator->Bind(zx::channel());
    DisplayCoordinatorListener listener(coordinator);

    EXPECT_FALSE(listener.valid());
  }
}

// Verify that DisplayCoordinator connects to the FIDL service.
TEST_F(DisplayCoordinatorListenerTest, Connect) {
  display_coordinator_listener()->InitializeCallbacks(/*on_invalid_cb=*/nullptr,
                                                      /*displays_changed_cb=*/nullptr,
                                                      /*client_ownership_change_cb=*/nullptr);

  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());
  RunLoopUntilIdle();
  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());
}

// Verify that DisplayCoordinator becomes invalid when the coordinator channel is closed.
TEST_F(DisplayCoordinatorListenerTest, DisconnectCoordinatorChannel) {
  uint on_invalid_count = 0;
  auto on_invalid_cb = [&on_invalid_count]() { on_invalid_count++; };
  display_coordinator_listener()->InitializeCallbacks(std::move(on_invalid_cb),
                                                      /*displays_changed_cb=*/nullptr,
                                                      /*client_ownership_change_cb=*/nullptr);

  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());
  RunLoopUntilIdle();
  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());

  mock_display_coordinator()->ResetCoordinatorBinding();
  RunLoopUntilIdle();
  EXPECT_EQ(1u, on_invalid_count);
  EXPECT_FALSE(display_coordinator_listener()->valid());

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

// Verify that DisplayCoordinator becomes invalid when the coordinator channel is closed, but that
// we don't receive a callback after ClearCallbacks.
TEST_F(DisplayCoordinatorListenerTest, DisconnectCoordinatorChannelAfterClearCallbacks) {
  uint on_invalid_count = 0;
  auto on_invalid_cb = [&on_invalid_count]() { on_invalid_count++; };
  display_coordinator_listener()->InitializeCallbacks(std::move(on_invalid_cb),
                                                      /*displays_changed_cb=*/nullptr,
                                                      /*client_ownership_change_cb=*/nullptr);

  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());
  RunLoopUntilIdle();
  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());
  display_coordinator_listener()->ClearCallbacks();
  mock_display_coordinator()->ResetCoordinatorBinding();
  RunLoopUntilIdle();
  EXPECT_EQ(0u, on_invalid_count);
  EXPECT_FALSE(display_coordinator_listener()->valid());
}

// Verify that DisplayCoordinator becomes invalid when the device and coordinator channel is closed,
// and that we don't get the callback twice.
TEST_F(DisplayCoordinatorListenerTest, DisconnectCoordinatorAndDeviceChannel) {
  uint on_invalid_count = 0;
  auto on_invalid_cb = [&on_invalid_count]() { on_invalid_count++; };
  display_coordinator_listener()->InitializeCallbacks(std::move(on_invalid_cb),
                                                      /*displays_changed_cb=*/nullptr,
                                                      /*client_ownership_change_cb=*/nullptr);

  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());
  RunLoopUntilIdle();
  EXPECT_TRUE(display_coordinator_listener()->valid());
  EXPECT_TRUE(mock_display_coordinator()->binding().is_bound());

  ResetMockDisplayCoordinator();
  RunLoopUntilIdle();
  EXPECT_EQ(1u, on_invalid_count);
  EXPECT_FALSE(display_coordinator_listener()->valid());

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

TEST_F(DisplayCoordinatorListenerTest, OnDisplaysChanged) {
  std::vector<fuchsia::hardware::display::Info> displays_added;
  std::vector<fuchsia::hardware::display::DisplayId> displays_removed;
  auto displays_changed_cb = [&displays_added, &displays_removed](
                                 std::vector<fuchsia::hardware::display::Info> added,
                                 std::vector<fuchsia::hardware::display::DisplayId> removed) {
    displays_added = added;
    displays_removed = removed;
  };

  display_coordinator_listener()->InitializeCallbacks(
      /*on_invalid_cb=*/nullptr, std::move(displays_changed_cb),
      /*client_ownership_change_cb=*/nullptr);
  fuchsia::hardware::display::Mode test_mode;
  test_mode.horizontal_resolution = 1024;
  test_mode.vertical_resolution = 800;
  test_mode.refresh_rate_e2 = 60;
  test_mode.flags = 0;
  fuchsia::hardware::display::Info test_display;
  test_display.id = {.value = 1};
  test_display.modes = {test_mode};
  test_display.pixel_format = {fuchsia::images2::PixelFormat::BGRA32};
  test_display.cursor_configs = {};
  test_display.manufacturer_name = "fake_manufacturer_name";
  test_display.monitor_name = "fake_monitor_name";
  test_display.monitor_serial = "fake_monitor_serial";

  mock_display_coordinator()->events().OnDisplaysChanged(/*added=*/{test_display},
                                                         /*removed=*/{{.value = 2u}});
  ASSERT_EQ(0u, displays_added.size());
  ASSERT_EQ(0u, displays_removed.size());
  RunLoopUntilIdle();
  ASSERT_EQ(1u, displays_added.size());
  ASSERT_EQ(1u, displays_removed.size());
  EXPECT_TRUE(fidl::Equals(displays_added[0], test_display));
  EXPECT_EQ(displays_removed[0].value, 2u);

  // Verify we stop getting callbacks after ClearCallbacks().
  display_coordinator_listener()->ClearCallbacks();
  mock_display_coordinator()->events().OnDisplaysChanged(/*added=*/{},
                                                         /*removed=*/{{.value = 3u}});
  RunLoopUntilIdle();

  // Expect that nothing changed.
  ASSERT_EQ(1u, displays_added.size());
  ASSERT_EQ(1u, displays_removed.size());
  EXPECT_EQ(displays_removed[0].value, 2u);

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

TEST_F(DisplayCoordinatorListenerTest, OnClientOwnershipChangeCallback) {
  bool has_ownership = false;
  auto client_ownership_change_cb = [&has_ownership](bool ownership) { has_ownership = ownership; };

  display_coordinator_listener()->InitializeCallbacks(
      /*on_invalid_cb=*/nullptr, /*displays_changed_cb=*/nullptr,
      std::move(client_ownership_change_cb));

  mock_display_coordinator()->events().OnClientOwnershipChange(true);
  EXPECT_FALSE(has_ownership);
  RunLoopUntilIdle();
  EXPECT_TRUE(has_ownership);

  // Verify we stop getting callbacks after ClearCallbacks().
  display_coordinator_listener()->ClearCallbacks();
  mock_display_coordinator()->events().OnClientOwnershipChange(false);
  RunLoopUntilIdle();
  // Expect that nothing changed.
  EXPECT_TRUE(has_ownership);

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

TEST_F(DisplayCoordinatorListenerTest, OnVsyncCallback) {
  fuchsia::hardware::display::DisplayId last_display_id = {
      .value = fuchsia::hardware::display::INVALID_DISP_ID};
  uint64_t last_timestamp = 0u;
  fuchsia::hardware::display::ConfigStamp last_config_stamp = {
      .value = fuchsia::hardware::display::INVALID_CONFIG_STAMP_VALUE};

  auto vsync_cb = [&](fuchsia::hardware::display::DisplayId display_id, uint64_t timestamp,
                      fuchsia::hardware::display::ConfigStamp stamp, uint64_t cookie) {
    last_display_id = display_id;
    last_timestamp = timestamp;
    last_config_stamp = std::move(stamp);
  };
  display_coordinator_listener()->InitializeCallbacks(/*on_invalid_cb=*/nullptr,
                                                      /*displays_changed_cb=*/nullptr,
                                                      /*client_ownership_change_cb=*/nullptr);
  display_coordinator_listener()->SetOnVsyncCallback(std::move(vsync_cb));

  constexpr fuchsia::hardware::display::DisplayId kTestDisplayId = {.value = 1};
  constexpr fuchsia::hardware::display::DisplayId kInvalidDisplayId = {.value = 2};
  const uint64_t kTestTimestamp = 111111u;
  const fuchsia::hardware::display::ConfigStamp kConfigStamp = {.value = 2u};
  mock_display_coordinator()->events().OnVsync(kTestDisplayId, kTestTimestamp, kConfigStamp, 0);
  ASSERT_EQ(fuchsia::hardware::display::INVALID_CONFIG_STAMP_VALUE, last_config_stamp.value);
  RunLoopUntilIdle();
  EXPECT_EQ(kTestDisplayId.value, last_display_id.value);
  EXPECT_EQ(kTestTimestamp, last_timestamp);
  EXPECT_EQ(last_config_stamp.value, kConfigStamp.value);

  // Verify we stop getting callbacks after ClearCallbacks().
  display_coordinator_listener()->ClearCallbacks();
  mock_display_coordinator()->events().OnVsync(kInvalidDisplayId, kTestTimestamp, kConfigStamp, 0);
  // Expect that nothing changed.
  RunLoopUntilIdle();
  EXPECT_EQ(kTestDisplayId.value, last_display_id.value);

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
