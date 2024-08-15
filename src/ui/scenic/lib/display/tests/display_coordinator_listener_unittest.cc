// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_coordinator_listener.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <fuchsia/images2/cpp/fidl.h>
#include <lib/fidl/cpp/comparison.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"

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
  void SetUp() override {
    ChannelPair coordinator_channel = CreateChannelPair();
    auto [listener_client, listener_server] =
        fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

    mock_display_coordinator_ =
        std::make_unique<MockDisplayCoordinator>(fuchsia::hardware::display::Info{});
    mock_display_coordinator_->Bind(std::move(coordinator_channel.server),
                                    std::move(listener_client));

    listener_server_end_ = std::move(listener_server);
  }

  DisplayCoordinatorListener* display_coordinator_listener() {
    return display_coordinator_listener_.get();
  }

  void ResetMockDisplayCoordinator() { mock_display_coordinator_.reset(); }
  void ResetDisplayCoordinatorListener() { display_coordinator_listener_.reset(); }

  MockDisplayCoordinator* mock_display_coordinator() { return mock_display_coordinator_.get(); }

  // Must be called no more than once per test case.
  fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> TakeListenerServerEnd() {
    FX_DCHECK(listener_server_end_.is_valid());
    return std::move(listener_server_end_);
  }

 private:
  std::unique_ptr<MockDisplayCoordinator> mock_display_coordinator_;
  std::unique_ptr<DisplayCoordinatorListener> display_coordinator_listener_;

  fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> listener_server_end_;
};

using DisplayCoordinatorListenerBasicTest = gtest::TestLoopFixture;

// Verify the documented constructor behavior doesn't cause any crash.
TEST_F(DisplayCoordinatorListenerBasicTest, ConstructorArgs) {
  auto [listener_client, listener_server] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  DisplayCoordinatorListener listener(std::move(listener_server), /*on_displays_changed=*/nullptr,
                                      /*on_vsync=*/nullptr, /*on_client_ownership_change=*/nullptr);
}

TEST_F(DisplayCoordinatorListenerTest, OnDisplaysChanged) {
  std::vector<fuchsia::hardware::display::Info> displays_added;
  std::vector<fuchsia::hardware::display::types::DisplayId> displays_removed;
  auto displays_changed_cb =
      [&displays_added, &displays_removed](
          std::vector<fuchsia::hardware::display::Info> added,
          std::vector<fuchsia::hardware::display::types::DisplayId> removed) {
        displays_added = added;
        displays_removed = removed;
      };

  DisplayCoordinatorListener listener(TakeListenerServerEnd(), std::move(displays_changed_cb),
                                      /*on_vsync=*/nullptr, /*on_client_ownership_change=*/nullptr);

  fuchsia_hardware_display::Mode test_mode = {{
      .horizontal_resolution = 1024,
      .vertical_resolution = 800,
      .refresh_rate_e2 = 60,
      .flags = 0,
  }};
  fuchsia_hardware_display::Info test_display = {{
      .id = fuchsia_hardware_display_types::DisplayId(1),
      .modes = {test_mode},
      .pixel_format = {fuchsia_images2::PixelFormat::kB8G8R8A8},
      .manufacturer_name = "fake_manufacturer_name",
      .monitor_name = "fake_monitor_name",
      .monitor_serial = "fake_monitor_serial",
  }};
  fit::result<fidl::OneWayError> result = mock_display_coordinator()->listener()->OnDisplaysChanged(
      {{.added = {test_display}, .removed = {2u}}});
  ASSERT_TRUE(result.is_ok());

  ASSERT_EQ(0u, displays_added.size());
  ASSERT_EQ(0u, displays_removed.size());
  RunLoopUntilIdle();
  ASSERT_EQ(1u, displays_added.size());
  ASSERT_EQ(1u, displays_removed.size());
  EXPECT_TRUE(fidl::Equals(displays_added[0], fidl::NaturalToHLCPP(test_display)));
  EXPECT_EQ(displays_removed[0].value, 2u);

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

TEST_F(DisplayCoordinatorListenerTest, OnClientOwnershipChangeCallback) {
  bool has_ownership = false;
  auto client_ownership_change_cb = [&has_ownership](bool ownership) { has_ownership = ownership; };

  DisplayCoordinatorListener listener(TakeListenerServerEnd(), /*on_displays_changed=*/nullptr,
                                      /*on_vsync=*/nullptr, std::move(client_ownership_change_cb));

  fit::result<fidl::OneWayStatus> result =
      mock_display_coordinator()->listener()->OnClientOwnershipChange(true);
  ASSERT_TRUE(result.is_ok());

  EXPECT_FALSE(has_ownership);
  RunLoopUntilIdle();
  EXPECT_TRUE(has_ownership);

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

TEST_F(DisplayCoordinatorListenerTest, OnVsyncCallback) {
  fuchsia::hardware::display::types::DisplayId last_display_id = {
      .value = fuchsia::hardware::display::types::INVALID_DISP_ID};
  uint64_t last_timestamp = 0u;
  fuchsia::hardware::display::types::ConfigStamp last_config_stamp = {
      .value = fuchsia::hardware::display::types::INVALID_CONFIG_STAMP_VALUE};

  auto vsync_cb = [&](fuchsia::hardware::display::types::DisplayId display_id, uint64_t timestamp,
                      fuchsia::hardware::display::types::ConfigStamp stamp, uint64_t cookie) {
    last_display_id = display_id;
    last_timestamp = timestamp;
    last_config_stamp = std::move(stamp);
  };
  DisplayCoordinatorListener listener(TakeListenerServerEnd(), /*on_displays_changed=*/nullptr,
                                      std::move(vsync_cb), /*on_client_ownership_change=*/nullptr);

  const fuchsia_hardware_display_types::DisplayId kTestDisplayId = {{.value = 1}};
  const fuchsia_hardware_display_types::DisplayId kInvalidDisplayId = {{.value = 2}};
  const zx_time_t kTestTimestamp = 111111;
  const fuchsia_hardware_display_types::ConfigStamp kConfigStamp = {{.value = 2u}};

  fit::result<fidl::OneWayStatus> result = mock_display_coordinator()->listener()->OnVsync({{
      .display_id = kTestDisplayId,
      .timestamp = kTestTimestamp,
      .applied_config_stamp = kConfigStamp,
      .cookie = 0,
  }});
  ASSERT_EQ(fuchsia::hardware::display::types::INVALID_CONFIG_STAMP_VALUE, last_config_stamp.value);
  RunLoopUntilIdle();
  EXPECT_EQ(kTestDisplayId.value(), last_display_id.value);
  EXPECT_EQ(kTestTimestamp, static_cast<zx_time_t>(last_timestamp));
  EXPECT_EQ(last_config_stamp.value, kConfigStamp.value());

  // Expect no crashes on teardown.
  ResetDisplayCoordinatorListener();
  RunLoopUntilIdle();
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
