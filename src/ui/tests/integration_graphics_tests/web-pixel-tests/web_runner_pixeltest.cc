// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/accessibility/semantics/cpp/fidl.h>
#include <fuchsia/buildinfo/cpp/fidl.h>
#include <fuchsia/component/cpp/fidl.h>
#include <fuchsia/fonts/cpp/fidl.h>
#include <fuchsia/input/injection/cpp/fidl.h>
#include <fuchsia/intl/cpp/fidl.h>
#include <fuchsia/kernel/cpp/fidl.h>
#include <fuchsia/memorypressure/cpp/fidl.h>
#include <fuchsia/metrics/cpp/fidl.h>
#include <fuchsia/net/interfaces/cpp/fidl.h>
#include <fuchsia/posix/socket/cpp/fidl.h>
#include <fuchsia/process/cpp/fidl.h>
#include <fuchsia/scheduler/cpp/fidl.h>
#include <fuchsia/sys/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/tracing/provider/cpp/fidl.h>
#include <fuchsia/ui/app/cpp/fidl.h>
#include <fuchsia/ui/display/singleton/cpp/fidl.h>
#include <fuchsia/ui/input/cpp/fidl.h>
#include <fuchsia/ui/policy/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <fuchsia/ui/test/input/cpp/fidl.h>
#include <fuchsia/vulkan/loader/cpp/fidl.h>
#include <fuchsia/web/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <algorithm>
#include <cmath>

#include <gtest/gtest.h>

#include "constants.h"
#include "fuchsia/component/decl/cpp/fidl.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/ui/testing/util/portable_ui_test.h"
#include "src/ui/testing/util/screenshot_helper.h"

namespace integration_tests {

using component_testing::ChildRef;
using component_testing::Directory;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::Route;

constexpr zx::duration kScreenshotTimeout = zx::sec(10);

enum class TapLocation { kTopLeft, kTopRight };

// A dot product of the coordinates of two pixels.
double Dot(const ui_testing::Pixel& v, const ui_testing::Pixel& u) {
  return static_cast<double>(v.red) * u.red + static_cast<double>(v.green) * u.green +
         static_cast<double>(v.blue) * u.blue + static_cast<double>(v.alpha) * u.alpha;
}

// Project v along the direction d.  d != 0.
double Project(const ui_testing::Pixel& v, const ui_testing::Pixel& dir) {
  double n = Dot(dir, dir);
  EXPECT_GT(n, 1e-10) << "Weakly conditioned result.";
  double p = Dot(v, dir);
  return p / n;
}

// Maximal sum of projections of pixels in h along the vector of pixel v.
//
// The idea is that at least some of the pixels in the histogram will be dominantly
// in the direction of the pixel v. But they don't need to be *exactly* in that
// direction, we're fine with an approximate direction. This accounts for variations
// in nuance up to a point. Should be enough for this test.
double MaxSumProject(const ui_testing::Pixel& v,
                     const std::vector<std::pair<uint32_t, ui_testing::Pixel>>& h) {
  double maxp = 0.0;
  for (const auto& elem : h) {
    auto n = elem.first;
    auto p = elem.second;

    maxp = std::max(maxp, Project(p, v) * n);
  }
  return maxp;
}

class WebRunnerPixelTest : public ui_testing::PortableUITest,
                           public ::testing::WithParamInterface<bool> {
 public:
  void SetUp() override {
    ui_testing::PortableUITest::SetUp();
    EXPECT_TRUE(realm_root().has_value());
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
      fit::function<bool(std::map<ui_testing::Pixel, uint32_t>)> histogram_predicate,
      zx::duration timeout = kScreenshotTimeout) {
    return RunLoopWithTimeoutOrUntil(
        [this, &histogram_predicate] {
          auto screenshot = TakeScreenshot();
          auto histogram = screenshot.Histogram();

          return histogram_predicate(std::move(histogram));
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
    // Add child components.
    for (auto [child, url] : GetTestComponents()) {
      realm_builder().AddChild(child, url, {.startup_mode = component_testing::StartupMode::EAGER});
    }

    // Add routes between components.
    for (auto route : GetWebEngineRoutes(ChildRef{kWebClient})) {
      realm_builder().AddRoute(route);
    }

    // Route the html code to the chromium client.
    realm_builder().InitMutableConfigToEmpty(kWebClient);
    realm_builder().SetConfigValue(kWebClient, "html", HtmlForTestCase());
    realm_builder().SetConfigValue(kWebClient, "use_vulkan",
                                   component_testing::ConfigValue::Bool(GetParam()));
  }

  virtual std::string HtmlForTestCase() = 0;

  std::vector<std::pair<std::string, std::string>> GetTestComponents() {
    return {
        std::make_pair(kBuildInfoProvider, kBuildInfoProviderUrl),
        std::make_pair(kFontsProvider, kFontsProviderUrl),
        std::make_pair(kIntl, kIntlUrl),
        std::make_pair(kMemoryPressureProvider, kMemoryPressureProviderUrl),
        std::make_pair(kMockCobalt, kMockCobaltUrl),
        std::make_pair(kNetstack, kNetstackUrl),
        std::make_pair(kWebClient, kWebClientUrl),
        std::make_pair(kTextManager, kTextManagerUrl),
        std::make_pair(kWebContextProvider, kWebContextProviderUrl),
        std::make_pair(kHttpServer, kHttpServerUrl),
    };
  }

  static std::vector<Route> GetWebEngineRoutes(ChildRef target) {
    return {{.capabilities = {Protocol{fuchsia::ui::composition::Screenshot::Name_},
                              Protocol{fuchsia::ui::display::singleton::Info::Name_}},
             .source = kTestUIStackRef,
             .targets = {ParentRef{}}},
            {.capabilities = {Protocol{fuchsia::fonts::Provider::Name_}},
             .source = ChildRef{kFontsProvider},
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::ui::input::ImeService::Name_}},
             .source = ChildRef{kTextManager},
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::memorypressure::Provider::Name_}},
             .source = ChildRef{kMemoryPressureProvider},
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::net::interfaces::State::Name_}},
             .source = ChildRef{kNetstack},
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::accessibility::semantics::SemanticsManager::Name_},
                              Protocol{fuchsia::ui::scenic::Scenic::Name_}},
             .source = kTestUIStackRef,
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::ui::composition::Flatland::Name_},
                              Protocol{fuchsia::ui::composition::Allocator::Name_}},
             .source = kTestUIStackRef,
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::web::ContextProvider::Name_}},
             .source = ChildRef{kWebContextProvider},
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::sys::Environment::Name_},
                              Protocol{fuchsia::logger::LogSink::Name_},
                              Protocol{fuchsia::media::ProfileProvider::Name_},
                              Protocol{fuchsia::media::AudioDeviceEnumerator::Name_}},
             .source = ParentRef(),
             .targets = {target, ChildRef{kWebContextProvider}, ChildRef{kHttpServer}}},
            {.capabilities = {Protocol{fuchsia::tracing::provider::Registry::Name_}},
             .source = ParentRef(),
             .targets = {ChildRef{kFontsProvider}}},
            {.capabilities = {Protocol{fuchsia::metrics::MetricEventLoggerFactory::Name_}},
             .source = ChildRef{kMockCobalt},
             .targets = {ChildRef{kMemoryPressureProvider}}},
            {.capabilities = {Protocol{fuchsia::sysmem::Allocator::Name_},
                              Protocol{fuchsia::vulkan::loader::Loader::Name_}},
             .source = ParentRef(),
             .targets = {ChildRef{kMemoryPressureProvider}, target}},
            {.capabilities = {Protocol{fuchsia::kernel::RootJobForInspect::Name_},
                              Protocol{fuchsia::kernel::Stats::Name_},
                              Protocol{fuchsia::scheduler::ProfileProvider::Name_},
                              Protocol{fuchsia::tracing::provider::Registry::Name_}},
             .source = ParentRef(),
             .targets = {ChildRef{kMemoryPressureProvider}}},
            {.capabilities = {Protocol{fuchsia::posix::socket::Provider::Name_}},
             .source = ChildRef{kNetstack},
             .targets = {target, ChildRef{kHttpServer}}},
            {.capabilities = {Protocol{fuchsia::buildinfo::Provider::Name_}},
             .source = ChildRef{kBuildInfoProvider},
             .targets = {target, ChildRef{kWebContextProvider}}},
            {
                .capabilities = {Directory{
                    .name = "root-ssl-certificates",
                    .type = fuchsia::component::decl::DependencyType::STRONG,
                }},
                .source = ParentRef{},
                .targets = {ChildRef{kWebContextProvider}},
            },
            {
                .capabilities =
                    {
                        Protocol{fuchsia::process::Launcher::Name_},
                        Protocol{fuchsia::vulkan::loader::Loader::Name_},
                    },
                .source = ParentRef{},
                .targets = {ChildRef{kWebClient}},
            },
            {.capabilities = {Protocol{fuchsia::intl::PropertyProvider::Name_}},
             .source = ChildRef{kIntl},
             .targets = {target}},
            {.capabilities = {Protocol{fuchsia::ui::app::ViewProvider::Name_}},
             .source = ChildRef{kWebClient},
             .targets = {ParentRef()}},
            {.capabilities =
                 {
                     Protocol{fuchsia::tracing::provider::Registry::Name_},
                     Protocol{fuchsia::logger::LogSink::Name_},
                 },
             .source = ParentRef(),
             .targets = {ChildRef{kFontsProvider}}}};
  }

  static constexpr auto kWebClient = "chromium_pixel_client";
  static constexpr auto kWebClientUrl = "#meta/chromium_pixel_client.cm";

  static constexpr auto kFontsProvider = "fonts_provider";
  static constexpr auto kFontsProviderUrl = "#meta/font_provider_hermetic_for_test.cm";

  static constexpr auto kTextManager = "text_manager";
  static constexpr auto kTextManagerUrl = "#meta/text_manager.cm";

  static constexpr auto kIntl = "intl";
  static constexpr auto kIntlUrl = "#meta/intl_property_manager.cm";

  static constexpr auto kMemoryPressureProvider = "memory_pressure_provider";
  static constexpr auto kMemoryPressureProviderUrl = "#meta/memory_monitor.cm";

  static constexpr auto kNetstack = "netstack";
  static constexpr auto kNetstackUrl = "#meta/netstack.cm";

  static constexpr auto kWebContextProvider = "web_context_provider";
  static constexpr auto kWebContextProviderUrl =
      "fuchsia-pkg://fuchsia.com/web_engine#meta/context_provider.cm";

  static constexpr auto kBuildInfoProvider = "build_info_provider";
  static constexpr auto kBuildInfoProviderUrl = "#meta/fake_build_info.cm";

  static constexpr auto kMockCobalt = "cobalt";
  static constexpr auto kMockCobaltUrl = "#meta/mock_cobalt.cm";

  static constexpr auto kHttpServer = "http_server";
  static constexpr auto kHttpServerUrl = "#meta/http_server.cm";

  fuchsia::ui::composition::ScreenshotPtr screenshotter_;
  fuchsia::ui::display::singleton::InfoPtr info_;
};

// Displays a non interactive HTML page with a solid red background.
class StaticHtmlPixelTests : public WebRunnerPixelTest {
 private:
  std::string HtmlForTestCase() override {
    return fxl::StringPrintf("http://localhost:%d/%s", kPort, kStaticHtml);
  }
};

INSTANTIATE_TEST_SUITE_P(ParameterizedStaticHtmlPixelTests, StaticHtmlPixelTests,
                         ::testing::Bool());

TEST_P(StaticHtmlPixelTests, ValidPixelTest) {
  LaunchClient();
  const auto num_pixels = display_width_ * display_height_;

  // TODO(fxb/116631): Find a better replacement for screenshot loops to verify that content has
  // been rendered on the display. Take screenshot until we see the web page's background color.
  ASSERT_TRUE(TakeScreenshotUntil([num_pixels](std::map<ui_testing::Pixel, uint32_t> histogram) {
    return histogram[ui_testing::Screenshot::kRed] == num_pixels;
  }));
}

// Displays a HTML web page with a solid magenta color. The color of the web page changes to blue
// on a tap event.
class DynamicHtmlPixelTests : public WebRunnerPixelTest {
 public:
  void SetUp() override {
    WebRunnerPixelTest::SetUp();
    RegisterTouchScreen();
  }

  void InjectInput(TapLocation tap_location) {
    auto touch = std::make_unique<fuchsia::ui::input::TouchscreenReport>();
    switch (tap_location) {
      case TapLocation::kTopLeft:
        InjectTapWithRetry(/* x = */ display_width_ / 4, /* y = */ display_height_ / 4);
        break;
      case TapLocation::kTopRight:
        InjectTapWithRetry(/* x = */ 3 * display_width_ / 4, /* y = */ display_height_ / 4);
        break;
      default:
        FX_NOTREACHED();
    }
  }

 private:
  std::string HtmlForTestCase() override {
    return fxl::StringPrintf("http://localhost:%d/%s", kPort, kDynamicHtml);
  }
};

INSTANTIATE_TEST_SUITE_P(ParameterizedDynamicHtmlPixelTests, DynamicHtmlPixelTests,
                         ::testing::Bool());

TEST_P(DynamicHtmlPixelTests, ValidPixelTest) {
  LaunchClient();
  const auto num_pixels = display_width_ * display_height_;

  // The web page should have a magenta background color.
  {
    ASSERT_TRUE(TakeScreenshotUntil([num_pixels](std::map<ui_testing::Pixel, uint32_t> histogram) {
      return histogram[ui_testing::Screenshot::kMagenta] == num_pixels;
    }));
  }

  InjectInput(TapLocation::kTopLeft);

  // The background color of the web page should change to blue after receiving a tap event.
  {
    ASSERT_TRUE(TakeScreenshotUntil([num_pixels](std::map<ui_testing::Pixel, uint32_t> histogram) {
      return histogram[ui_testing::Screenshot::kBlue] == num_pixels;
    }));
  }
}

std::vector<std::pair<uint32_t, ui_testing::Pixel>> TopPixels(
    std::map<ui_testing::Pixel, uint32_t> histogram) {
  std::vector<std::pair<uint32_t, ui_testing::Pixel>> vec;
  std::transform(histogram.begin(), histogram.end(), std::inserter(vec, vec.begin()),
                 [](const std::pair<ui_testing::Pixel, uint32_t> p) {
                   return std::make_pair(p.second, p.first);
                 });
  std::stable_sort(vec.begin(), vec.end(),
                   [](const auto& a, const auto& b) { return a.first > b.first; });

  std::vector<std::pair<uint32_t, ui_testing::Pixel>> top;
  std::copy(vec.begin(), vec.begin() + std::min<ptrdiff_t>(vec.size(), 10),
            std::back_inserter(top));
  return top;
}

// This test renders a video in the browser and takes a screenshot to verify the pixels. The video
// displays a scene as shown below:-
//  __________________________________
// |                |                |
// |     Yellow     |        Red     |
// |                |                |
// |________________|________________|
// |                |                |
// |                |                |
// |      Blue      |     Green      |
// |________________|________________|
class VideoHtmlPixelTests : public WebRunnerPixelTest {
 private:
  std::string HtmlForTestCase() override {
    return fxl::StringPrintf("http://localhost:%d/%s", kPort, kVideoHtml);
  }
};

INSTANTIATE_TEST_SUITE_P(ParameterizedVideoHtmlPixelTests, VideoHtmlPixelTests, ::testing::Bool());

TEST_P(VideoHtmlPixelTests, ValidPixelTest) {
  // BGRA values,
  const ui_testing::Pixel kYellow = {0, 255, 255, 255};
  const ui_testing::Pixel kRed = {0, 0, 255, 255};
  const ui_testing::Pixel kBlue = {255, 0, 0, 255};
  const ui_testing::Pixel kGreen = {0, 255, 0, 255};
  const ui_testing::Pixel kBackground = {255, 255, 255, 255};

  LaunchClient();

  // The web page should render the scene as shown above.
  // TODO(fxb/116631): Find a better replacement for screenshot loops to verify that content has
  // been rendered on the display.
  ASSERT_TRUE(TakeScreenshotUntil([&](std::map<ui_testing::Pixel, uint32_t> histogram) {
    // Have at least some visual feedback about the examined histogram.
    auto top = TopPixels(histogram);
    std::cout << "Histogram top:" << std::endl;
    for (const auto& elems : top) {
      std::cout << "{ " << elems.second << " value: " << elems.first << " }" << std::endl;
    }
    std::cout << "--------------" << std::endl;

    // Fail the predicate check until at least some pixels are kinda blue. This will
    // cause a re-attempt of a screenshot.
    if (MaxSumProject(kBlue, top) < 60000) {
      return false;
    }

    // Video's background color should not be visible.
    EXPECT_LT(histogram[kBackground], 10u);

    // Note that we do not see pure colors in the video but a shade of the colors shown in the
    // diagram. Since it is hard to assert on the exact number of pixels for each shade of the
    // color, the test asserts on whether the shade that's most like the given color is
    // prominent enough.
    EXPECT_GT(MaxSumProject(kYellow, top), 100000);
    EXPECT_GT(MaxSumProject(kRed, top), 100000);
    EXPECT_GT(MaxSumProject(kBlue, top), 100000);
    EXPECT_GT(MaxSumProject(kGreen, top), 100000);

    return true;
  }));
}

}  // namespace integration_tests
