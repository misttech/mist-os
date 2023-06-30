// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_controller.h"

#include <fuchsia/hardware/display/cpp/fidl.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/ui/scenic/lib/display/tests/mock_display_controller.h"

namespace scenic_impl {
namespace display {
namespace test {

class DisplayCoordinatorTest : public gtest::TestLoopFixture {};

TEST_F(DisplayCoordinatorTest, Display2Test) {
  constexpr fuchsia::hardware::display::DisplayId kDisplayId = {.value = 2};
  const fuchsia::hardware::display::Mode kDisplayMode = {
      .horizontal_resolution = 1024, .vertical_resolution = 800, .refresh_rate_e2 = 60, .flags = 0};
  const fuchsia_images2::PixelFormat kPixelFormat = fuchsia_images2::PixelFormat::kBgra32;

  Display2 display(kDisplayId, /*display_modes=*/{kDisplayMode}, /*pixel_formats=*/{kPixelFormat});

  EXPECT_EQ(kDisplayId.value, display.display_id().value);
  EXPECT_TRUE(fidl::Equals(kDisplayMode, display.display_modes()[0]));
  EXPECT_EQ(kPixelFormat, display.pixel_formats()[0]);

  display.OnVsync(zx::time(1), {.value = 1});
  bool invoked_vsync_callback = false;
  display.set_on_vsync_callback(
      [&](zx::time timestamp, fuchsia::hardware::display::ConfigStamp stamp) {
        invoked_vsync_callback = true;
        EXPECT_EQ(zx::time(2), timestamp);
        EXPECT_EQ(2u, stamp.value);
      });
  EXPECT_FALSE(invoked_vsync_callback);
  display.OnVsync(zx::time(2), {.value = 2});
  EXPECT_TRUE(invoked_vsync_callback);
}

TEST_F(DisplayCoordinatorTest, DisplayCoordinatorTest) {
  DisplayCoordinatorObjects display_controller_objs = CreateMockDisplayCoordinator();

  constexpr fuchsia::hardware::display::DisplayId kDisplayId1 = {.value = 1};
  constexpr fuchsia::hardware::display::DisplayId kDisplayId2 = {.value = 2};
  const fuchsia::hardware::display::Mode kDisplayMode = {
      .horizontal_resolution = 1024, .vertical_resolution = 800, .refresh_rate_e2 = 60, .flags = 0};
  const fuchsia_images2::PixelFormat kPixelFormat = fuchsia_images2::PixelFormat::kBgra32;

  Display2 display1(kDisplayId1, {kDisplayMode}, {kPixelFormat});
  Display2 display2(kDisplayId2, {kDisplayMode}, {kPixelFormat});

  std::vector<Display2> displays;
  displays.push_back(std::move(display1));
  DisplayCoordinator dc(std::move(displays), display_controller_objs.interface_ptr);

  EXPECT_EQ(display_controller_objs.interface_ptr.get(), dc.coordinator().get());

  EXPECT_EQ(1u, dc.displays()->size());
  EXPECT_EQ(kDisplayId1.value, dc.displays()->at(0).display_id().value);

  bool display_removed = false;
  dc.set_on_display_removed_callback([&](fuchsia::hardware::display::DisplayId display_id) {
    display_removed = true;
    EXPECT_EQ(kDisplayId1.value, display_id.value);
  });

  bool display_added = false;
  dc.set_on_display_added_callback([&](Display2* display) {
    display_added = true;
    EXPECT_EQ(kDisplayId2.value, display->display_id().value);
  });

  dc.AddDisplay(std::move(display2));

  EXPECT_TRUE(display_added);
  EXPECT_EQ(2u, dc.displays()->size());
  EXPECT_EQ(kDisplayId2.value, dc.displays()->at(1).display_id().value);

  dc.RemoveDisplay(kDisplayId1);
  EXPECT_EQ(1u, dc.displays()->size());
  EXPECT_EQ(kDisplayId2.value, dc.displays()->at(0).display_id().value);
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
