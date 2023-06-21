// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/accessibility/semantics/cpp/fidl.h>
#include <fuchsia/buildinfo/cpp/fidl.h>
#include <fuchsia/component/cpp/fidl.h>
#include <fuchsia/fonts/cpp/fidl.h>
#include <fuchsia/input/report/cpp/fidl.h>
#include <fuchsia/kernel/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <fuchsia/memorypressure/cpp/fidl.h>
#include <fuchsia/metrics/cpp/fidl.h>
#include <fuchsia/net/interfaces/cpp/fidl.h>
#include <fuchsia/posix/socket/cpp/fidl.h>
#include <fuchsia/process/cpp/fidl.h>
#include <fuchsia/scheduler/cpp/fidl.h>
#include <fuchsia/session/scene/cpp/fidl.h>
#include <fuchsia/sys/cpp/fidl.h>
#include <fuchsia/tracing/provider/cpp/fidl.h>
#include <fuchsia/ui/app/cpp/fidl.h>
#include <fuchsia/ui/input/cpp/fidl.h>
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
#include <zircon/types.h>
#include <zircon/utc.h>

#include <cstddef>
#include <cstdint>
#include <iostream>
#include <memory>
#include <optional>
#include <queue>
#include <string>
#include <type_traits>
#include <utility>
#include <vector>

#include <gtest/gtest.h>

#include "lib/fidl/cpp/interface_ptr.h"
#include "src/ui/testing/util/portable_ui_test.h"

namespace {

// Types imported for the realm_builder library.
using component_testing::ChildRef;
using component_testing::ConfigValue;
using component_testing::Directory;
using component_testing::LocalComponentHandles;
using component_testing::LocalComponentImpl;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::Realm;
using component_testing::Route;

// Alias for Component child name as provided to Realm Builder.
using ChildName = std::string;

// Maximum pointer movement during a clickpad press for the gesture to
// be guaranteed to be interpreted as a click. For movement greater than
// this value, upper layers may, e.g., interpret the gesture as a drag.
//
// This value corresponds to the one used to instantiate the ClickDragHandler
// registered by Input Pipeline in Scene Manager.
constexpr int64_t kClickToDragThreshold = 16.0;

// Combines all vectors in `vecs` into one.
template <typename T>
std::vector<T> merge(std::initializer_list<std::vector<T>> vecs) {
  std::vector<T> result;
  for (auto v : vecs) {
    result.insert(result.end(), v.begin(), v.end());
  }
  return result;
}

int ButtonsToInt(const std::vector<fuchsia::ui::test::input::MouseButton>& buttons) {
  int result = 0;
  for (const auto& button : buttons) {
    result |= (0x1 >> button);
  }

  return result;
}

// Contains the current mouse input state.
//
// Used to be part of MouseInputListenerServer. The state is now externalized,
// because the new AddLocalChild API does not allow directly inspecting the
// state of the local child component itself.  Instead, this state is shared
// between the test fixture and the local component below.
class MouseInputState {
 public:
  size_t SizeOfEvents() const { return events_.size(); }

  fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest PopEvent() {
    auto e = std::move(events_.front());
    events_.pop();
    return e;
  }

  const fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest& LastEvent() const {
    return events_.back();
  }

  void ClearEvents() { events_ = {}; }

 private:
  friend class MouseInputListenerServer;

  std::queue<fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest> events_;
};

// `MouseInputListener` is a local test protocol that our test apps use to let us know
// what position and button press state the mouse cursor has.
class MouseInputListenerServer : public fuchsia::ui::test::input::MouseInputListener,
                                 public LocalComponentImpl {
 public:
  explicit MouseInputListenerServer(async_dispatcher_t* dispatcher,
                                    std::weak_ptr<MouseInputState> state)
      : dispatcher_(dispatcher), state_(std::move(state)) {}

  void ReportMouseInput(
      fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest request) override {
    if (auto s = state_.lock()) {
      s->events_.push(std::move(request));
    }
  }

  // When the component framework requests for this component to start, this
  // method will be invoked by the realm_builder library.
  void OnStart() override {
    FX_CHECK(outgoing()->AddPublicService(
                 fidl::InterfaceRequestHandler<fuchsia::ui::test::input::MouseInputListener>(
                     [this](auto request) {
                       bindings_.AddBinding(this, std::move(request), dispatcher_);
                     })) == ZX_OK);
  }

 private:
  // Not owned.
  async_dispatcher_t* dispatcher_ = nullptr;
  fidl::BindingSet<fuchsia::ui::test::input::MouseInputListener> bindings_;
  std::weak_ptr<MouseInputState> state_;
};

constexpr auto kMouseInputListener = "mouse_input_listener";

struct Position {
  double x = 0.0;
  double y = 0.0;
};

class MouseInputBase : public ui_testing::PortableUITest {
 protected:
  MouseInputBase() : mouse_state_(std::make_shared<MouseInputState>()) {}

  std::string GetTestUIStackUrl() override { return "#meta/test-ui-stack.cm"; }

  void SetUp() override {
    ui_testing::PortableUITest::SetUp();

    // Register fake mouse device.
    RegisterMouse();
  }

  void TearDown() override {
    ui_testing::PortableUITest::TearDown();

    // at the end of test, ensure event queue is empty.
    ASSERT_EQ(mouse_state_->SizeOfEvents(), 0u);
  }

  // Subclass should implement this method to add v2 components to the test realm
  // next to the base ones added.
  virtual std::vector<std::pair<ChildName, std::string>> GetTestComponents() { return {}; }

  // Subclass should implement this method to add capability routes to the test
  // realm next to the base ones added.
  virtual std::vector<Route> GetTestRoutes() { return {}; }

  // Helper method for checking the test.mouse.MouseInputListener response from the client app.
  void VerifyEvent(
      fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest& pointer_data,
      double expected_x, double expected_y,
      std::vector<fuchsia::ui::test::input::MouseButton> expected_buttons,
      const fuchsia::ui::test::input::MouseEventPhase expected_phase,
      const std::string& component_name) {
    FX_LOGS(INFO) << "Client received mouse change at (" << pointer_data.local_x() << ", "
                  << pointer_data.local_y() << ") with buttons "
                  << ButtonsToInt(pointer_data.buttons()) << ".";
    FX_LOGS(INFO) << "Expected mouse change is at approximately (" << expected_x << ", "
                  << expected_y << ") with buttons " << ButtonsToInt(expected_buttons) << ".";

    // Allow for minor rounding differences in coordinates.
    // Note: These approximations don't account for `PointerMotionDisplayScaleHandler`
    // or `PointerMotionSensorScaleHandler`. We will need to do so in order to validate
    // larger motion or different sized displays.
    EXPECT_NEAR(pointer_data.local_x(), expected_x, 1);
    EXPECT_NEAR(pointer_data.local_y(), expected_y, 1);
    EXPECT_EQ(pointer_data.buttons(), expected_buttons);
    EXPECT_EQ(pointer_data.phase(), expected_phase);
    EXPECT_EQ(pointer_data.component_name(), component_name);
  }

  void VerifyEventLocationOnTheRightOfExpectation(
      fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest& pointer_data,
      double expected_x_min, double expected_y,
      std::vector<fuchsia::ui::test::input::MouseButton> expected_buttons,
      const fuchsia::ui::test::input::MouseEventPhase expected_phase,
      const std::string& component_name) {
    FX_LOGS(INFO) << "Client received mouse change at (" << pointer_data.local_x() << ", "
                  << pointer_data.local_y() << ") with buttons "
                  << ButtonsToInt(pointer_data.buttons()) << ".";
    FX_LOGS(INFO) << "Expected mouse change is at approximately (>" << expected_x_min << ", "
                  << expected_y << ") with buttons " << ButtonsToInt(expected_buttons) << ".";

    EXPECT_GT(pointer_data.local_x(), expected_x_min);
    EXPECT_NEAR(pointer_data.local_y(), expected_y, 1);
    EXPECT_EQ(pointer_data.buttons(), expected_buttons);
    EXPECT_EQ(pointer_data.phase(), expected_phase);
    EXPECT_EQ(pointer_data.component_name(), component_name);
  }

  void ExtendRealm() override {
    // Key part of service setup: have this test component vend the
    // |MouseInputListener| service in the constructed realm.
    auto* d = dispatcher();
    realm_builder().AddLocalChild(kMouseInputListener, [d, s = mouse_state_]() {
      return std::make_unique<MouseInputListenerServer>(d, s);
    });

    for (const auto& [name, component] : GetTestComponents()) {
      realm_builder().AddChild(name, component);
    }

    // Add the necessary routing for each of the extra components added above.
    for (const auto& route : GetTestRoutes()) {
      realm_builder().AddRoute(route);
    }
  }

  std::shared_ptr<MouseInputState> mouse_state_;

  // Override test-ui-stack config.
  bool use_flatland() override { return true; }

  // Use a DPR other than 1.0, so that logical and physical coordinate spaces
  // are different.
  float device_pixel_ratio() override { return 2.f; }

  static constexpr auto kFontsProvider = "fonts_provider";
  static constexpr auto kFontsProviderUrl = "#meta/font_provider_hermetic_for_test.cm";
};

class ChromiumInputTest : public MouseInputBase {
 protected:
  std::vector<std::pair<ChildName, std::string>> GetTestComponents() override {
    return {
        std::make_pair(kMouseInputChromium, kMouseInputChromiumUrl),
        std::make_pair(kBuildInfoProvider, kBuildInfoProviderUrl),
        std::make_pair(kFontsProvider, kFontsProviderUrl),
        std::make_pair(kMemoryPressureProvider, kMemoryPressureProviderUrl),
        std::make_pair(kNetstack, kNetstackUrl),
        std::make_pair(kMockCobalt, kMockCobaltUrl),
        std::make_pair(kWebContextProvider, kWebContextProviderUrl),
    };
  }

  std::vector<Route> GetTestRoutes() override {
    return merge({GetChromiumRoutes(ChildRef{kMouseInputChromium}),
                  {
                      {.capabilities = {Protocol{fuchsia::ui::app::ViewProvider::Name_}},
                       .source = ChildRef{kMouseInputChromium},
                       .targets = {ParentRef()}},
                  }});
  }

  // Routes needed to setup Chromium client.
  static std::vector<Route> GetChromiumRoutes(ChildRef target) {
    return {
        {.capabilities =
             {
                 Protocol{fuchsia::accessibility::semantics::SemanticsManager::Name_},
                 Protocol{fuchsia::ui::composition::Allocator::Name_},
                 Protocol{fuchsia::ui::composition::Flatland::Name_},
                 Protocol{fuchsia::ui::scenic::Scenic::Name_},
             },
         .source = kTestUIStackRef,
         .targets = {target}},
        {.capabilities =
             {
                 Protocol{fuchsia::kernel::VmexResource::Name_},
                 Protocol{fuchsia::process::Launcher::Name_},
                 Protocol{fuchsia::vulkan::loader::Loader::Name_},
             },
         .source = ParentRef(),
         .targets = {target}},
        {
            .capabilities =
                {
                    Protocol{fuchsia::logger::LogSink::Name_},
                },
            .source = ParentRef(),
            .targets =
                {
                    target, ChildRef{kFontsProvider}, ChildRef{kMemoryPressureProvider},
                    ChildRef{kBuildInfoProvider}, ChildRef{kWebContextProvider},
                    ChildRef{kMockCobalt},
                    // Not including kNetstack here, since it emits spurious
                    // FATAL errors.
                },
        },
        {.capabilities = {Protocol{fuchsia::ui::test::input::MouseInputListener::Name_}},
         .source = ChildRef{kMouseInputListener},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::fonts::Provider::Name_}},
         .source = ChildRef{kFontsProvider},
         .targets = {target}},
        {.capabilities =
             {
                 Protocol{fuchsia::tracing::provider::Registry::Name_},
             },
         .source = ParentRef(),
         .targets = {target, ChildRef{kFontsProvider}}},
        {.capabilities = {Protocol{fuchsia::memorypressure::Provider::Name_}},
         .source = ChildRef{kMemoryPressureProvider},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::net::interfaces::State::Name_}},
         .source = ChildRef{kNetstack},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::web::ContextProvider::Name_}},
         .source = ChildRef{kWebContextProvider},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::metrics::MetricEventLoggerFactory::Name_}},
         .source = ChildRef{kMockCobalt},
         .targets = {ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::sysmem::Allocator::Name_}},
         .source = ParentRef(),
         .targets = {ChildRef{kMemoryPressureProvider}, target}},
        {.capabilities = {Protocol{fuchsia::scheduler::ProfileProvider::Name_}},
         .source = ParentRef(),
         .targets = {ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::kernel::RootJobForInspect::Name_}},
         .source = ParentRef(),
         .targets = {ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::kernel::Stats::Name_}},
         .source = ParentRef(),
         .targets = {ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::tracing::provider::Registry::Name_}},
         .source = ParentRef(),
         .targets = {ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::posix::socket::Provider::Name_}},
         .source = ChildRef{kNetstack},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::buildinfo::Provider::Name_}},
         .source = ChildRef{kBuildInfoProvider},
         .targets = {target}},
        {.capabilities = {Directory{
             .name = "root-ssl-certificates",
             .type = fuchsia::component::decl::DependencyType::STRONG,
         }},
         .source = ParentRef(),
         .targets = {ChildRef{kWebContextProvider}}},
    };
  }

  // TODO(fxbug.dev/58322): EnsureMouseIsReadyAndGetPosition will send a mouse click
  // (down and up) and wait for response to ensure the mouse is ready to use. We will retry a
  // mouse click if we can not get the mouseup response in small timeout. This function returns
  // the cursor position in WebEngine coordinate system.
  Position EnsureMouseIsReadyAndGetPosition() {
    for (int retry = 0; retry < kMaxRetry; retry++) {
      // Mouse down and up.
      SimulateMouseEvent(/* pressed_buttons = */ {fuchsia::ui::test::input::MouseButton::FIRST},
                         /* movement_x = */ 0, /* movement_y = */ 0);
      SimulateMouseEvent(/* pressed_buttons = */ {}, /* movement_x = */ 0, /* movement_y = */ 0);

      auto wait_until_last_event_phase_or_timeout =
          [this](fuchsia::ui::test::input::MouseEventPhase event_phase) {
            return RunLoopWithTimeoutOrUntil(
                [this, event_phase] {
                  return mouse_state_->SizeOfEvents() > 0 &&
                         mouse_state_->LastEvent().phase() == event_phase;
                },
                kFirstEventRetryInterval);
          };

      bool got_mouse_up =
          wait_until_last_event_phase_or_timeout(fuchsia::ui::test::input::MouseEventPhase::UP);

      if (got_mouse_up) {
        // There is an issue we found in retry that the mouse up we got may
        // come from previous retry loop, then EnsureMouseIsReadyAndGetPosition
        // exit and the down-up event caught by test body, and break the test
        // expectation. Here we inject 1 more wheel event, because wheel event
        // only injected once, wheel we receive the wheel event, we know all
        // events from EnsureMouseIsReadyAndGetPosition are processed.
        SimulateMouseScroll(/* pressed_buttons = */ {}, /* scroll_x = */ 0, /* scroll_y = */ 1);
        wait_until_last_event_phase_or_timeout(fuchsia::ui::test::input::MouseEventPhase::WHEEL);
        Position p;
        p.x = mouse_state_->LastEvent().local_x();
        p.y = mouse_state_->LastEvent().local_y();
        mouse_state_->ClearEvents();
        return p;
      }
    }

    FX_LOGS(FATAL) << "Can not get mouse click in max retries " << kMaxRetry;
    return Position{};
  }

  static constexpr auto kMouseInputChromium = "mouse-input-chromium";
  static constexpr auto kMouseInputChromiumUrl = "#meta/mouse-input-chromium.cm";

  static constexpr auto kWebContextProvider = "web_context_provider";
  static constexpr auto kWebContextProviderUrl =
      "fuchsia-pkg://fuchsia.com/web_engine#meta/context_provider.cm";

  static constexpr auto kMemoryPressureProvider = "memory_pressure_provider";
  static constexpr auto kMemoryPressureProviderUrl = "#meta/memory_monitor.cm";

  static constexpr auto kNetstack = "netstack";
  static constexpr auto kNetstackUrl = "#meta/netstack.cm";

  static constexpr auto kBuildInfoProvider = "build_info_provider";
  static constexpr auto kBuildInfoProviderUrl = "#meta/fake_build_info.cm";

  static constexpr auto kMockCobalt = "cobalt";
  static constexpr auto kMockCobaltUrl = "#meta/mock_cobalt.cm";

  // The first event to WebEngine may lost, see EnsureMouseIsReadyAndGetPosition. Retry to ensure
  // WebEngine is ready to process events.
  static constexpr auto kFirstEventRetryInterval = zx::sec(1);

  // To avoid retry to timeout, limit 10 retries, if still not ready, fail it with meaningful
  // error.
  static const int kMaxRetry = 10;
};

TEST_F(ChromiumInputTest, ChromiumMouseMove) {
  LaunchClient();

  auto initial_position = EnsureMouseIsReadyAndGetPosition();

  double initial_x = initial_position.x;
  double initial_y = initial_position.y;

  SimulateMouseEvent(/* pressed_buttons = */ {},
                     /* movement_x = */ 5, /* movement_y = */ 0);
  RunLoopUntil([this] { return mouse_state_->SizeOfEvents() == 1; });

  auto event_move = mouse_state_->PopEvent();

  VerifyEventLocationOnTheRightOfExpectation(
      event_move,
      /*expected_x_min=*/initial_x,
      /*expected_y=*/initial_y,
      /*expected_buttons=*/{},
      /*expected_type=*/fuchsia::ui::test::input::MouseEventPhase::MOVE,
      /*component_name=*/"mouse-input-chromium");
}

TEST_F(ChromiumInputTest, ChromiumMouseDownMoveUp) {
  LaunchClient();

  auto initial_position = EnsureMouseIsReadyAndGetPosition();

  double initial_x = initial_position.x;
  double initial_y = initial_position.y;

  SimulateMouseEvent(/* pressed_buttons = */ {fuchsia::ui::test::input::MouseButton::FIRST},
                     /* movement_x = */ 0, /* movement_y = */ 0);
  SimulateMouseEvent(/* pressed_buttons = */ {fuchsia::ui::test::input::MouseButton::FIRST},
                     /* movement_x = */ kClickToDragThreshold, /* movement_y = */ 0);
  SimulateMouseEvent(/* pressed_buttons = */ {}, /* movement_x = */ 0, /* movement_y = */ 0);

  RunLoopUntil([this] { return mouse_state_->SizeOfEvents() == 3; });

  auto event_down = mouse_state_->PopEvent();
  auto event_move = mouse_state_->PopEvent();
  auto event_up = mouse_state_->PopEvent();

  VerifyEvent(event_down,
              /*expected_x=*/initial_x,
              /*expected_y=*/initial_y,
              /*expected_buttons=*/{fuchsia::ui::test::input::MouseButton::FIRST},
              /*expected_type=*/fuchsia::ui::test::input::MouseEventPhase::DOWN,
              /*component_name=*/"mouse-input-chromium");
  VerifyEventLocationOnTheRightOfExpectation(
      event_move,
      /*expected_x_min=*/initial_x,
      /*expected_y=*/initial_y,
      /*expected_buttons=*/{fuchsia::ui::test::input::MouseButton::FIRST},
      /*expected_type=*/fuchsia::ui::test::input::MouseEventPhase::MOVE,
      /*component_name=*/"mouse-input-chromium");
  VerifyEvent(event_up,
              /*expected_x=*/event_move.local_x(),
              /*expected_y=*/initial_y,
              /*expected_buttons=*/{},
              /*expected_type=*/fuchsia::ui::test::input::MouseEventPhase::UP,
              /*component_name=*/"mouse-input-chromium");
}

TEST_F(ChromiumInputTest, ChromiumMouseWheel) {
  LaunchClient();

  auto initial_position = EnsureMouseIsReadyAndGetPosition();

  double initial_x = initial_position.x;
  double initial_y = initial_position.y;

  SimulateMouseScroll(/* pressed_buttons = */ {}, /* scroll_x = */ 1, /* scroll_y = */ 0);
  RunLoopUntil([this] { return mouse_state_->SizeOfEvents() == 1; });

  auto event_wheel_h = mouse_state_->PopEvent();

  VerifyEvent(event_wheel_h,
              /*expected_x=*/initial_x,
              /*expected_y=*/initial_y,
              /*expected_buttons=*/{},
              /*expected_type=*/fuchsia::ui::test::input::MouseEventPhase::WHEEL,
              /*component_name=*/"mouse-input-chromium");
  // Chromium will scale the count of ticks to pixel.
  // Positive delta in Fuchsia  means scroll left, and scroll left in JS is negative delta.
  EXPECT_LT(event_wheel_h.wheel_x_physical_pixel(), 0);
  EXPECT_EQ(event_wheel_h.wheel_y_physical_pixel(), 0);

  SimulateMouseScroll(/* pressed_buttons = */ {}, /* scroll_x = */ 0, /* scroll_y = */ 1);
  RunLoopUntil([this] { return mouse_state_->SizeOfEvents() == 1; });

  auto event_wheel_v = mouse_state_->PopEvent();

  VerifyEvent(event_wheel_v,
              /*expected_x=*/initial_x,
              /*expected_y=*/initial_y,
              /*expected_buttons=*/{},
              /*expected_type=*/fuchsia::ui::test::input::MouseEventPhase::WHEEL,
              /*component_name=*/"mouse-input-chromium");
  // Chromium will scale the count of ticks to pixel.
  // Positive delta in Fuchsia means scroll up, and scroll up in JS is negative delta.
  EXPECT_LT(event_wheel_v.wheel_y_physical_pixel(), 0);
  EXPECT_EQ(event_wheel_v.wheel_x_physical_pixel(), 0);
}

}  // namespace
