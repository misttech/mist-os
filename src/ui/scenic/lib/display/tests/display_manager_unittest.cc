// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_manager.h"

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <fuchsia/images2/cpp/fidl.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/default.h>
#include <lib/async/time.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>

#include <unordered_set>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"
#include "src/ui/scenic/lib/utils/range_inclusive.h"

namespace scenic_impl {
namespace gfx {
namespace test {

namespace {

fidl::Endpoints<fuchsia_hardware_display::Coordinator> CreateCoordinatorEndpoints() {
  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> endpoints_result =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  FX_CHECK(endpoints_result.is_ok())
      << "Failed to create endpoints: " << endpoints_result.status_string();
  return std::move(endpoints_result.value());
}

}  // namespace

class DisplayManagerMockTest : public gtest::TestLoopFixture {
 public:
  // |testing::Test|
  void SetUp() override {
    TestLoopFixture::SetUp();

    async_set_default_dispatcher(dispatcher());
    display_manager_ = std::make_unique<display::DisplayManager>([]() {});
  }

  // |testing::Test|
  void TearDown() override {
    display_manager_.reset();
    TestLoopFixture::TearDown();
  }

  display::DisplayManager* display_manager() { return display_manager_.get(); }
  display::Display* display() { return display_manager()->default_display(); }

 private:
  std::unique_ptr<display::DisplayManager> display_manager_;
};

TEST_F(DisplayManagerMockTest, DisplayVsyncCallback) {
  constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  const uint32_t kDisplayWidth = 1024;
  const uint32_t kDisplayHeight = 768;
  const size_t kTotalVsync = 10;
  const size_t kAcknowledgeRate = 5;

  std::unordered_set<uint64_t> cookies_sent;
  size_t num_vsync_display_received = 0;
  size_t num_vsync_acknowledgement = 0;

  auto coordinator_channel = CreateCoordinatorEndpoints();

  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  display_manager()->SetDefaultDisplayForTests(
      std::make_shared<display::Display>(kDisplayId, kDisplayWidth, kDisplayHeight));

  display::test::MockDisplayCoordinator mock_display_coordinator(
      fuchsia::hardware::display::Info{});
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());
  mock_display_coordinator.set_acknowledge_vsync_fn(
      [&cookies_sent, &num_vsync_acknowledgement](uint64_t cookie) {
        ASSERT_TRUE(cookies_sent.find(cookie) != cookies_sent.end());
        ++num_vsync_acknowledgement;
      });

  display_manager()->default_display()->SetVsyncCallback(
      [&num_vsync_display_received](zx::time timestamp,
                                    fuchsia::hardware::display::types::ConfigStamp stamp) {
        ++num_vsync_display_received;
      });

  for (size_t vsync_id = 1; vsync_id <= kTotalVsync; vsync_id++) {
    // We only require acknowledgement for every |kAcknowledgeRate| Vsync IDs.
    uint64_t cookie = (vsync_id % kAcknowledgeRate == 0) ? vsync_id : 0;

    test_loop().AdvanceTimeByEpsilon();
    mock_display_coordinator.events().OnVsync(kDisplayId, /* timestamp */ test_loop().Now().get(),
                                              {.value = 1u}, cookie);
    if (cookie) {
      cookies_sent.insert(cookie);
    }

    // Display coordinator should handle the incoming Vsync message.
    EXPECT_TRUE(RunLoopUntilIdle());
  }

  EXPECT_EQ(num_vsync_display_received, kTotalVsync);
  EXPECT_EQ(num_vsync_acknowledgement, kTotalVsync / kAcknowledgeRate);
}

TEST_F(DisplayManagerMockTest, OnDisplayAdded) {
  static constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  static constexpr int kDisplayWidth = 1024;
  static constexpr int kDisplayHeight = 768;
  static constexpr int kDisplayRefreshRateHz = 60;
  static const std::vector<fuchsia::images2::PixelFormat> kHlcppSupportedPixelFormats = {
      fuchsia::images2::PixelFormat::R8G8B8A8,
  };
  static const std::vector<fuchsia_images2::PixelFormat> kNaturalSupportedPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8,
  };

  auto coordinator_channel = CreateCoordinatorEndpoints();
  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  const fuchsia::hardware::display::Info kDisplayInfo = {
      .id = kDisplayId,
      .modes =
          {
              fuchsia::hardware::display::Mode{
                  .horizontal_resolution = kDisplayWidth,
                  .vertical_resolution = kDisplayHeight,
                  .refresh_rate_e2 = kDisplayRefreshRateHz * 100,
              },
          },
      .pixel_format = kHlcppSupportedPixelFormats,
      .cursor_configs = {},
      .manufacturer_name = "manufacturer",
      .monitor_name = "model",
      .monitor_serial = "0001",
      .horizontal_size_mm = 120,
      .vertical_size_mm = 100,
      .using_fallback_size = false,
  };
  display::test::MockDisplayCoordinator mock_display_coordinator(kDisplayInfo);
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());
  mock_display_coordinator.SendOnDisplayChangedEvent();

  EXPECT_TRUE(RunLoopUntilIdle());

  const display::Display* default_display = display_manager()->default_display();
  ASSERT_TRUE(default_display != nullptr);
  EXPECT_EQ(default_display->width_in_px(), static_cast<uint32_t>(kDisplayWidth));
  EXPECT_EQ(default_display->height_in_px(), static_cast<uint32_t>(kDisplayHeight));
  EXPECT_EQ(default_display->maximum_refresh_rate_in_millihertz(),
            static_cast<uint32_t>(kDisplayRefreshRateHz * 1'000));
  EXPECT_THAT(default_display->pixel_formats(),
              testing::ElementsAreArray(kNaturalSupportedPixelFormats));
}

TEST_F(DisplayManagerMockTest, SelectPreferredMode) {
  static constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  static constexpr fuchsia::hardware::display::Mode kPreferredMode = {
      .horizontal_resolution = 1024,
      .vertical_resolution = 768,
      .refresh_rate_e2 = 60'00,
  };
  static constexpr fuchsia::hardware::display::Mode kNonPreferredMode = {
      .horizontal_resolution = 800,
      .vertical_resolution = 600,
      .refresh_rate_e2 = 30'00,
  };
  static const std::vector<fuchsia::images2::PixelFormat> kHlcppSupportedPixelFormats = {
      fuchsia::images2::PixelFormat::R8G8B8A8,
  };
  static const std::vector<fuchsia_images2::PixelFormat> kNaturalSupportedPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8,
  };

  auto coordinator_channel = CreateCoordinatorEndpoints();
  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  const fuchsia::hardware::display::Info kDisplayInfo = {
      .id = kDisplayId,
      .modes =
          {
              kPreferredMode,
              kNonPreferredMode,
          },
      .pixel_format = kHlcppSupportedPixelFormats,
      .cursor_configs = {},
      .manufacturer_name = "manufacturer",
      .monitor_name = "model",
      .monitor_serial = "0001",
      .horizontal_size_mm = 120,
      .vertical_size_mm = 100,
      .using_fallback_size = false,
  };

  display::test::MockDisplayCoordinator mock_display_coordinator(kDisplayInfo);
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());
  mock_display_coordinator.SendOnDisplayChangedEvent();

  EXPECT_TRUE(RunLoopUntilIdle());

  const display::Display* default_display = display_manager()->default_display();
  ASSERT_TRUE(default_display != nullptr);

  EXPECT_EQ(default_display->width_in_px(), kPreferredMode.horizontal_resolution);
  EXPECT_EQ(default_display->height_in_px(), kPreferredMode.vertical_resolution);
  EXPECT_EQ(default_display->maximum_refresh_rate_in_millihertz(),
            kPreferredMode.refresh_rate_e2 * 10);
}

TEST(DisplayManager, ICanHazDisplayMode) {
  static constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  static constexpr fuchsia::hardware::display::Mode kPreferredMode = {
      .horizontal_resolution = 1024,
      .vertical_resolution = 768,
      .refresh_rate_e2 = 60'00,
  };
  static constexpr fuchsia::hardware::display::Mode kNonPreferredButSelectedMode = {
      .horizontal_resolution = 800,
      .vertical_resolution = 600,
      .refresh_rate_e2 = 30'00,
  };
  static const std::vector<fuchsia::images2::PixelFormat> kHlcppSupportedPixelFormats = {
      fuchsia::images2::PixelFormat::R8G8B8A8,
  };
  static const std::vector<fuchsia_images2::PixelFormat> kNaturalSupportedPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8,
  };

  async::TestLoop loop;
  async_set_default_dispatcher(loop.dispatcher());

  const fuchsia::hardware::display::Info kDisplayInfo = {
      .id = kDisplayId,
      .modes =
          {
              kPreferredMode,
              kNonPreferredButSelectedMode,
          },
      .pixel_format = kHlcppSupportedPixelFormats,
      .cursor_configs = {},
      .manufacturer_name = "manufacturer",
      .monitor_name = "model",
      .monitor_serial = "0001",
      .horizontal_size_mm = 120,
      .vertical_size_mm = 100,
      .using_fallback_size = false,
  };

  auto coordinator_channel = CreateCoordinatorEndpoints();
  display::test::MockDisplayCoordinator mock_display_coordinator(kDisplayInfo);
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());

  display::DisplayManager display_manager(/*i_can_haz_display_id=*/std::nullopt,
                                          /*display_mode_index_override=*/std::make_optional(1),
                                          display::DisplayModeConstraints{},
                                          /*display_available_cb=*/[]() {});
  display_manager.BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  mock_display_coordinator.SendOnDisplayChangedEvent();

  EXPECT_TRUE(loop.RunUntilIdle());

  const display::Display* default_display = display_manager.default_display();
  ASSERT_TRUE(default_display != nullptr);

  EXPECT_EQ(default_display->width_in_px(), kNonPreferredButSelectedMode.horizontal_resolution);
  EXPECT_EQ(default_display->height_in_px(), kNonPreferredButSelectedMode.vertical_resolution);
  EXPECT_EQ(default_display->maximum_refresh_rate_in_millihertz(),
            kNonPreferredButSelectedMode.refresh_rate_e2 * 10);
}

TEST(DisplayManager, DisplayModeConstraintsHorizontalResolution) {
  static const display::DisplayModeConstraints kDisplayModeConstraints = {
      .width_px_range = utils::RangeInclusive(700, 900),
  };

  static constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  static constexpr fuchsia::hardware::display::Mode kModeNotSatisfyingConstraints = {
      .horizontal_resolution = 1024,
      .vertical_resolution = 768,
      .refresh_rate_e2 = 60'00,
  };
  static constexpr fuchsia::hardware::display::Mode kModeSatisfyingConstraints = {
      .horizontal_resolution = 800,
      .vertical_resolution = 600,
      .refresh_rate_e2 = 30'00,
  };
  static const std::vector<fuchsia::images2::PixelFormat> kHlcppSupportedPixelFormats = {
      fuchsia::images2::PixelFormat::R8G8B8A8,
  };
  static const std::vector<fuchsia_images2::PixelFormat> kNaturalSupportedPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8,
  };

  async::TestLoop loop;
  async_set_default_dispatcher(loop.dispatcher());

  const fuchsia::hardware::display::Info kDisplayInfo = {
      .id = kDisplayId,
      .modes =
          {
              kModeNotSatisfyingConstraints,
              kModeSatisfyingConstraints,
          },
      .pixel_format = kHlcppSupportedPixelFormats,
      .cursor_configs = {},
      .manufacturer_name = "manufacturer",
      .monitor_name = "model",
      .monitor_serial = "0001",
      .horizontal_size_mm = 120,
      .vertical_size_mm = 100,
      .using_fallback_size = false,
  };

  auto coordinator_channel = CreateCoordinatorEndpoints();
  display::test::MockDisplayCoordinator mock_display_coordinator(kDisplayInfo);
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());

  display::DisplayManager display_manager(/*i_can_haz_display_id=*/std::nullopt,
                                          /*display_mode_index_override=*/std::nullopt,
                                          kDisplayModeConstraints,
                                          /*display_available_cb=*/[]() {});
  display_manager.BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  mock_display_coordinator.SendOnDisplayChangedEvent();

  EXPECT_TRUE(loop.RunUntilIdle());

  const display::Display* default_display = display_manager.default_display();
  ASSERT_TRUE(default_display != nullptr);

  EXPECT_EQ(default_display->width_in_px(), kModeSatisfyingConstraints.horizontal_resolution);
  EXPECT_EQ(default_display->height_in_px(), kModeSatisfyingConstraints.vertical_resolution);
  EXPECT_EQ(default_display->maximum_refresh_rate_in_millihertz(),
            kModeSatisfyingConstraints.refresh_rate_e2 * 10);
}

TEST(DisplayManager, DisplayModeConstraintsVerticalResolution) {
  static const display::DisplayModeConstraints kDisplayModeConstraints = {
      .height_px_range = utils::RangeInclusive(500, 700),
  };

  static constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  static constexpr fuchsia::hardware::display::Mode kModeNotSatisfyingConstraints = {
      .horizontal_resolution = 1024,
      .vertical_resolution = 768,
      .refresh_rate_e2 = 60'00,
  };
  static constexpr fuchsia::hardware::display::Mode kModeSatisfyingConstraints = {
      .horizontal_resolution = 800,
      .vertical_resolution = 600,
      .refresh_rate_e2 = 30'00,
  };
  static const std::vector<fuchsia::images2::PixelFormat> kHlcppSupportedPixelFormats = {
      fuchsia::images2::PixelFormat::R8G8B8A8,
  };
  static const std::vector<fuchsia_images2::PixelFormat> kNaturalSupportedPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8,
  };

  async::TestLoop loop;
  async_set_default_dispatcher(loop.dispatcher());

  const fuchsia::hardware::display::Info kDisplayInfo = {
      .id = kDisplayId,
      .modes =
          {
              kModeNotSatisfyingConstraints,
              kModeSatisfyingConstraints,
          },
      .pixel_format = kHlcppSupportedPixelFormats,
      .cursor_configs = {},
      .manufacturer_name = "manufacturer",
      .monitor_name = "model",
      .monitor_serial = "0001",
      .horizontal_size_mm = 120,
      .vertical_size_mm = 100,
      .using_fallback_size = false,
  };

  auto coordinator_channel = CreateCoordinatorEndpoints();
  display::test::MockDisplayCoordinator mock_display_coordinator(kDisplayInfo);
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());

  display::DisplayManager display_manager(/*i_can_haz_display_id=*/std::nullopt,
                                          /*display_mode_index_override=*/std::nullopt,
                                          kDisplayModeConstraints,
                                          /*display_available_cb=*/[]() {});
  display_manager.BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  mock_display_coordinator.SendOnDisplayChangedEvent();

  EXPECT_TRUE(loop.RunUntilIdle());

  const display::Display* default_display = display_manager.default_display();
  ASSERT_TRUE(default_display != nullptr);

  EXPECT_EQ(default_display->width_in_px(), kModeSatisfyingConstraints.horizontal_resolution);
  EXPECT_EQ(default_display->height_in_px(), kModeSatisfyingConstraints.vertical_resolution);
  EXPECT_EQ(default_display->maximum_refresh_rate_in_millihertz(),
            kModeSatisfyingConstraints.refresh_rate_e2 * 10);
}

TEST(DisplayManager, DisplayModeConstraintsRefreshRateLimit) {
  static const display::DisplayModeConstraints kDisplayModeConstraints = {
      .refresh_rate_millihertz_range = utils::RangeInclusive(20'000, 50'000),
  };

  static constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  static constexpr fuchsia::hardware::display::Mode kModeNotSatisfyingConstraints = {
      .horizontal_resolution = 1024,
      .vertical_resolution = 768,
      .refresh_rate_e2 = 60'00,
  };
  static constexpr fuchsia::hardware::display::Mode kModeSatisfyingConstraints = {
      .horizontal_resolution = 800,
      .vertical_resolution = 600,
      .refresh_rate_e2 = 30'00,
  };
  static const std::vector<fuchsia::images2::PixelFormat> kHlcppSupportedPixelFormats = {
      fuchsia::images2::PixelFormat::R8G8B8A8,
  };
  static const std::vector<fuchsia_images2::PixelFormat> kNaturalSupportedPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8,
  };

  async::TestLoop loop;
  async_set_default_dispatcher(loop.dispatcher());

  const fuchsia::hardware::display::Info kDisplayInfo = {
      .id = kDisplayId,
      .modes =
          {
              kModeNotSatisfyingConstraints,
              kModeSatisfyingConstraints,
          },
      .pixel_format = kHlcppSupportedPixelFormats,
      .cursor_configs = {},
      .manufacturer_name = "manufacturer",
      .monitor_name = "model",
      .monitor_serial = "0001",
      .horizontal_size_mm = 120,
      .vertical_size_mm = 100,
      .using_fallback_size = false,
  };

  auto coordinator_channel = CreateCoordinatorEndpoints();
  display::test::MockDisplayCoordinator mock_display_coordinator(kDisplayInfo);
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());

  display::DisplayManager display_manager(/*i_can_haz_display_id=*/std::nullopt,
                                          /*display_mode_index_override=*/std::nullopt,
                                          kDisplayModeConstraints,
                                          /*display_available_cb=*/[]() {});
  display_manager.BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  mock_display_coordinator.SendOnDisplayChangedEvent();

  EXPECT_TRUE(loop.RunUntilIdle());

  const display::Display* default_display = display_manager.default_display();
  ASSERT_TRUE(default_display != nullptr);

  EXPECT_EQ(default_display->width_in_px(), kModeSatisfyingConstraints.horizontal_resolution);
  EXPECT_EQ(default_display->height_in_px(), kModeSatisfyingConstraints.vertical_resolution);
  EXPECT_EQ(default_display->maximum_refresh_rate_in_millihertz(),
            kModeSatisfyingConstraints.refresh_rate_e2 * 10);
}

TEST(DisplayManager, DisplayModeConstraintsOverriddenByModeIndex) {
  static const display::DisplayModeConstraints kDisplayModeConstraints = {
      .width_px_range = utils::RangeInclusive(700, 900),
  };

  static constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  static constexpr fuchsia::hardware::display::Mode kModeNotSatisfyingConstraints = {
      .horizontal_resolution = 1024,
      .vertical_resolution = 768,
      .refresh_rate_e2 = 60'00,
  };
  static constexpr fuchsia::hardware::display::Mode kModeSatisfyingConstraints = {
      .horizontal_resolution = 800,
      .vertical_resolution = 600,
      .refresh_rate_e2 = 30'00,
  };
  static constexpr fuchsia::hardware::display::Mode kModeOverridden = {
      .horizontal_resolution = 1280,
      .vertical_resolution = 960,
      .refresh_rate_e2 = 30'00,
  };
  static const std::vector<fuchsia::images2::PixelFormat> kHlcppSupportedPixelFormats = {
      fuchsia::images2::PixelFormat::R8G8B8A8,
  };
  static const std::vector<fuchsia_images2::PixelFormat> kNaturalSupportedPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8,
  };

  async::TestLoop loop;
  async_set_default_dispatcher(loop.dispatcher());

  const fuchsia::hardware::display::Info kDisplayInfo = {
      .id = kDisplayId,
      .modes =
          {
              kModeNotSatisfyingConstraints,
              kModeSatisfyingConstraints,
              kModeOverridden,
          },
      .pixel_format = kHlcppSupportedPixelFormats,
      .cursor_configs = {},
      .manufacturer_name = "manufacturer",
      .monitor_name = "model",
      .monitor_serial = "0001",
      .horizontal_size_mm = 120,
      .vertical_size_mm = 100,
      .using_fallback_size = false,
  };

  auto coordinator_channel = CreateCoordinatorEndpoints();
  display::test::MockDisplayCoordinator mock_display_coordinator(kDisplayInfo);
  mock_display_coordinator.Bind(coordinator_channel.server.TakeChannel());

  display::DisplayManager display_manager(/*i_can_haz_display_id=*/std::nullopt,
                                          /*display_mode_index_override=*/std::make_optional(2),
                                          kDisplayModeConstraints,
                                          /*display_available_cb=*/[]() {});
  display_manager.BindDefaultDisplayCoordinator(std::move(coordinator_channel.client));

  mock_display_coordinator.SendOnDisplayChangedEvent();

  EXPECT_TRUE(loop.RunUntilIdle());

  const display::Display* default_display = display_manager.default_display();
  ASSERT_TRUE(default_display != nullptr);

  EXPECT_EQ(default_display->width_in_px(), kModeOverridden.horizontal_resolution);
  EXPECT_EQ(default_display->height_in_px(), kModeOverridden.vertical_resolution);
  EXPECT_EQ(default_display->maximum_refresh_rate_in_millihertz(),
            kModeOverridden.refresh_rate_e2 * 10);
}

}  // namespace test
}  // namespace gfx
}  // namespace scenic_impl
