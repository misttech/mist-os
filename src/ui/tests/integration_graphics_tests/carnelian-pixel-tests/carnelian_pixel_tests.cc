// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/logger/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/tracing/provider/cpp/fidl.h>
#include <fuchsia/ui/app/cpp/fidl.h>
#include <fuchsia/ui/display/singleton/cpp/fidl.h>
#include <fuchsia/ui/input3/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <fuchsia/vulkan/loader/cpp/fidl.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <gtest/gtest.h>

#include "src/ui/testing/util/portable_ui_test.h"
#include "src/ui/testing/util/screenshot_helper.h"

namespace integration_tests {

using component_testing::ChildRef;
using component_testing::ParentRef;
using component_testing::Protocol;

constexpr zx::duration kScreenshotTimeout = zx::sec(10);

// Launches a carnelian app which draws a solid filled square on the display and takes a screenshot
// to validate content.
class CarnelianPixelTest : public ui_testing::PortableUITest {
 public:
  void SetUp() override {
    ui_testing::PortableUITest::SetUp();

    screenshotter_ = realm_root()->component().Connect<fuchsia::ui::composition::Screenshot>();

    // Get display information.
    info_ = realm_root()->component().Connect<fuchsia::ui::display::singleton::Info>();
    bool has_completed = false;
    info_->GetMetrics([this, &has_completed](auto info) {
      display_width_ = info.extent_in_px().width;
      display_height_ = info.extent_in_px().height;
      has_completed = true;
    });

    RunLoopUntil([&has_completed] { return has_completed; });
  }

  bool TakeScreenshotUntil(
      ui_testing::Pixel color,
      fit::function<void(std::map<ui_testing::Pixel, uint32_t>)> histogram_predicate = nullptr,
      zx::duration timeout = kScreenshotTimeout) {
    return RunLoopWithTimeoutOrUntil(
        [this, &histogram_predicate, &color] {
          auto screenshot = TakeScreenshot();
          auto histogram = screenshot.Histogram();

          bool color_found = histogram[color] > 0;
          if (color_found && histogram_predicate != nullptr) {
            histogram_predicate(std::move(histogram));
          }
          return color_found;
        },
        timeout);
  }

  ui_testing::Screenshot TakeScreenshot() {
    FX_LOGS(INFO) << "Taking screenshot... ";

    fuchsia::ui::composition::ScreenshotTakeRequest request;
    request.set_format(fuchsia::ui::composition::ScreenshotFormat::BGRA_RAW);

    std::optional<fuchsia::ui::composition::ScreenshotTakeResponse> response;
    screenshotter_->Take(std::move(request), [this, &response](auto screenshot) {
      response = std::move(screenshot);
      QuitLoop();
    });

    FX_LOGS(INFO) << "Screenshot captured.";

    EXPECT_FALSE(RunLoopWithTimeout(kScreenshotTimeout)) << "Timed out waiting for screenshot.";

    return ui_testing::Screenshot(response->vmo(), display_width_, display_height_,
                                  0 /*display_rotation*/);
  }

  bool use_flatland() override { return true; }
  std::string GetTestUIStackUrl() override { return "#meta/test-ui-stack.cm"; }

  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;

 private:
  void ExtendRealm() override {
    // Add the carnelian component.
    realm_builder().AddChild(kCarnelianClient, kCarnelianClientUrl);

    // Add routes between components.
    realm_builder().AddRoute(
        {.capabilities = {Protocol{fuchsia::ui::composition::Screenshot::Name_},
                          Protocol{fuchsia::ui::display::singleton::Info::Name_}},
         .source = kTestUIStackRef,
         .targets = {ParentRef{}}});

    realm_builder().AddRoute(
        {.capabilities = {Protocol{fuchsia::logger::LogSink::Name_},
                          Protocol{fuchsia::sysmem::Allocator::Name_},
                          Protocol{fuchsia::tracing::provider::Registry::Name_},
                          Protocol{fuchsia::vulkan::loader::Loader::Name_}},
         .source = ParentRef(),
         .targets = {ChildRef{kCarnelianClient}}});

    realm_builder().AddRoute({.capabilities = {Protocol{fuchsia::ui::composition::Flatland::Name_},
                                               Protocol{fuchsia::ui::composition::Allocator::Name_},
                                               Protocol{fuchsia::ui::scenic::Scenic::Name_},
                                               Protocol{fuchsia::ui::input3::Keyboard::Name_}},
                              .source = kTestUIStackRef,
                              .targets = {ChildRef{kCarnelianClient}}});

    realm_builder().AddRoute({.capabilities = {Protocol{fuchsia::ui::app::ViewProvider::Name_}},
                              .source = ChildRef{kCarnelianClient},
                              .targets = {ParentRef()}});
  }

  static constexpr auto kCarnelianClient = "carnelian_client";
  static constexpr auto kCarnelianClientUrl = "#meta/static_square.cm";

  fuchsia::ui::composition::ScreenshotPtr screenshotter_;
  fuchsia::ui::display::singleton::InfoPtr info_;
};

// The carnelian app draws a scene as shown below:-
// The square is drawn in the center of the display with length |display_height_/2|.
//   __________________________________
//  |                                 |
//  |              BLUE               |
//  |            ---------            |
//  |           |         |           |
//  |           | MAGENTA |           |
//  |            ---------            |
//  |                                 |
//  |_________________________________|
TEST_F(CarnelianPixelTest, ValidPixelTest) {
  const uint32_t total_pixels = display_width_ * display_height_;
  const uint32_t square_pixels = display_height_ / 2 * display_height_ / 2;
  const uint32_t background_pixels = total_pixels - square_pixels;

  LaunchClient();

  // Take screenshots till the background color shows up.
  ASSERT_TRUE(TakeScreenshotUntil(
      ui_testing::Screenshot::kBlue, [&](std::map<ui_testing::Pixel, uint32_t> histogram) {
        // TODO(fxb/116631): Switch to exact comparisons after Astro precision issues are resolved.
        EXPECT_NEAR(histogram[ui_testing::Screenshot::kMagenta], square_pixels, display_width_);
        EXPECT_NEAR(histogram[ui_testing::Screenshot::kBlue], background_pixels, display_width_);
      }));
}

}  // namespace integration_tests
