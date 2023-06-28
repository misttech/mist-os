// Copyright 2022 The Fuchsia Authors. All rights reserved.
//
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/accessibility/semantics/cpp/fidl.h>
#include <fuchsia/buildinfo/cpp/fidl.h>
#include <fuchsia/cobalt/cpp/fidl.h>
#include <fuchsia/component/cpp/fidl.h>
#include <fuchsia/feedback/cpp/fidl.h>
#include <fuchsia/fonts/cpp/fidl.h>
#include <fuchsia/intl/cpp/fidl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <fuchsia/kernel/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <fuchsia/memorypressure/cpp/fidl.h>
#include <fuchsia/metrics/cpp/fidl.h>
#include <fuchsia/net/interfaces/cpp/fidl.h>
#include <fuchsia/posix/socket/cpp/fidl.h>
#include <fuchsia/process/cpp/fidl.h>
#include <fuchsia/scheduler/cpp/fidl.h>
#include <fuchsia/session/scene/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/tracing/provider/cpp/fidl.h>
#include <fuchsia/ui/app/cpp/fidl.h>
#include <fuchsia/ui/input/cpp/fidl.h>
#include <fuchsia/ui/input3/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <fuchsia/ui/test/input/cpp/fidl.h>
#include <fuchsia/ui/test/scene/cpp/fidl.h>
#include <fuchsia/vulkan/loader/cpp/fidl.h>
#include <fuchsia/web/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <zircon/status.h>
#include <zircon/types.h>
#include <zircon/utc.h>

#include <cstddef>
#include <cstdint>
#include <iostream>
#include <memory>
#include <type_traits>
#include <utility>
#include <vector>

#include <gtest/gtest.h>
#include <src/ui/testing/ui_test_manager/ui_test_manager.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/input/testing/fake_input_report_device/fake.h"
#include "src/ui/input/testing/fake_input_report_device/reports_reader.h"

namespace {

// Types imported for the realm_builder library.
using component_testing::ChildRef;
using component_testing::Directory;
using component_testing::LocalComponentHandles;
using component_testing::LocalComponentImpl;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::Realm;
using component_testing::RealmBuilder;
using component_testing::RealmRoot;
using component_testing::Route;

using ChildName = std::string;

// Max timeout in failure cases.
// Set this as low as you can that still works across all test platforms.
constexpr zx::duration kTimeout = zx::min(5);

// Combines all vectors in `vecs` into one.
template <typename T>
std::vector<T> merge(std::initializer_list<std::vector<T>> vecs) {
  std::vector<T> result;
  for (auto v : vecs) {
    result.insert(result.end(), v.begin(), v.end());
  }
  return result;
}

// Externalized test specific keyboard input state.
//
// A shsared instance of this object is kept in the test fixture, and also
// injected into KeyboardInputListener below.
class KeyboardInputState {
 public:
  // If true, the remote end of the connection sent the `ReportReady` signal.
  bool IsReady() const { return ready_; }

  // Returns true if the last response received matches `expected`.  If a match is found,
  // the match is consumed, so a next call to HasResponse starts from scratch.
  bool HasResponse(const std::string& expected) {
    bool match = response_.has_value() && response_.value() == expected;
    if (match) {
      response_ = std::nullopt;
    }
    return match;
  }

  // Same as above, except we are looking for a substring.
  bool ResponseContains(const std::string& substring) {
    bool match = response_.has_value() && response_.value().find(substring) != std::string::npos;
    if (match) {
      response_ = std::nullopt;
    }
    return match;
  }

 private:
  friend class KeyboardInputListenerServer;

  std::optional<std::string> response_ = std::nullopt;
  bool ready_ = false;
};

// `KeyboardInputListener` is a test protocol that our test app uses to let us know
// what text is being entered into its only text field.
//
// The text field contents are reported on almost every change, so if you are entering a long
// text, you will see calls corresponding to successive additions of characters, not just the
// end result.
class KeyboardInputListenerServer : public fuchsia::ui::test::input::KeyboardInputListener,
                                    public LocalComponentImpl {
 public:
  KeyboardInputListenerServer(async_dispatcher_t* dispatcher,
                              std::weak_ptr<KeyboardInputState> state)
      : dispatcher_(dispatcher), state_(std::move(state)) {}

  KeyboardInputListenerServer(const KeyboardInputListenerServer&) = delete;
  KeyboardInputListenerServer& operator=(const KeyboardInputListenerServer&) = delete;

  // |fuchsia::ui::test::input::KeyboardInputListener|
  void ReportTextInput(
      fuchsia::ui::test::input::KeyboardInputListenerReportTextInputRequest request) override {
    FX_LOGS(INFO) << "App sent: '" << request.text() << "'";
    if (auto s = state_.lock()) {
      s->response_ = request.text();
    }
  }

  // |fuchsia::ui::test::input::KeyboardInputListener|
  void ReportReady(ReportReadyCallback callback) override {
    if (auto s = state_.lock()) {
      s->ready_ = true;
    }
    callback();
  }

  // Starts this server.
  void OnStart() override {
    ASSERT_EQ(ZX_OK, outgoing()->AddPublicService(bindings_.GetHandler(this, dispatcher_)));
  }

 private:
  // Not owned.
  async_dispatcher_t* dispatcher_ = nullptr;
  fidl::BindingSet<fuchsia::ui::test::input::KeyboardInputListener> bindings_;
  std::weak_ptr<KeyboardInputState> state_;
};

constexpr auto kResponseListener = "test_text_response_listener";

// See README.md for instructions on how to run this test with chrome remote
// devtools, which is super-useful for debugging.
class ChromiumInputBase : public gtest::RealLoopFixture {
 protected:
  static constexpr auto kTapRetryInterval = zx::sec(1);

  ChromiumInputBase() : keyboard_input_state_(std::make_shared<KeyboardInputState>()) {}

  sys::ServiceDirectory* realm_exposed_services() { return realm_exposed_services_.get(); }

  void SetUp() override {
    // Post a "just in case" quit task, if the test hangs.
    async::PostDelayedTask(
        dispatcher(),
        [] { FX_LOGS(FATAL) << "\n\n>> Test did not complete in time, terminating.  <<\n\n"; },
        kTimeout);

    ui_testing::UITestRealm::Config config;
    config.use_flatland = true;
    config.use_scene_owner = true;
    config.accessibility_owner = ui_testing::UITestRealm::AccessibilityOwnerType::FAKE;
    config.passthrough_capabilities = {
        {// Uncomment the configuration below if you want to run Chrome remote
         // devtools. See README.md for details.
         // Protocol{fuchsia::posix::socket::Provider::Name_},
         // Protocol{fuchsia::net::interfaces::State::Name_},
         // TODO(fxbug.dev/123550): Do the feedback protocols need to be here? Is
         // including launch_context_provider.shard.cml sufficient?
         Protocol{fuchsia::kernel::Stats::Name_}, Protocol{fuchsia::kernel::VmexResource::Name_},
         Protocol{fuchsia::process::Launcher::Name_},
         Protocol{fuchsia::feedback::ComponentDataRegister::Name_},
         Protocol{fuchsia::feedback::CrashReportingProductRegister::Name_},
         Directory{
             .name = "root-ssl-certificates",
             .type = fuchsia::component::decl::DependencyType::STRONG,
         }},
    };
    config.ui_to_client_services = {
        fuchsia::accessibility::semantics::SemanticsManager::Name_,
        fuchsia::ui::composition::Allocator::Name_,
        fuchsia::ui::composition::Flatland::Name_,
        fuchsia::ui::input3::Keyboard::Name_,
        fuchsia::ui::input::ImeService::Name_,
        fuchsia::ui::scenic::Scenic::Name_,
    };
    ui_test_manager_.emplace(std::move(config));
    AssembleRealm(GetTestComponents(), GetTestRoutes());

    // Get the display dimensions.
    FX_LOGS(INFO) << "Waiting for scenic display info";
    auto scenic = realm_exposed_services()->Connect<fuchsia::ui::scenic::Scenic>();
    scenic->GetDisplayInfo([this](fuchsia::ui::gfx::DisplayInfo display_info) {
      display_width_ = display_info.width_in_px;
      display_height_ = display_info.height_in_px;
      FX_LOGS(INFO) << "Got display_width = " << display_width()
                    << " and display_height = " << display_height();
    });
    RunLoopUntil([this] { return display_width_.has_value() && display_height_.has_value(); });

    input_registry_ = realm_exposed_services()->Connect<fuchsia::ui::test::input::Registry>();
    input_registry_.set_error_handler([](zx_status_t status) {
      FX_LOGS(FATAL) << "Error from input helper: " << zx_status_get_string(status);
    });

    RegisterTouchScreen();
    RegisterKeyboard();
  }

  void TearDown() override {
    // We're about to shut down the realm; unbind to unhook the error handler.
    input_registry_.Unbind();
    bool complete = false;
    ui_test_manager_->TeardownRealm(
        [&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });
  }

  // Subclass should implement this method to add v2 components to the test realm
  // next to the base ones added.
  virtual std::vector<std::pair<ChildName, std::string>> GetTestComponents() { return {}; }

  // Subclass should implement this method to add capability routes to the test
  // realm next to the base ones added.
  virtual std::vector<Route> GetTestRoutes() { return {}; }

  void RegisterKeyboard() {
    FX_CHECK(input_registry_.is_bound()) << "input_registry_ is not initialized";
    FX_LOGS(INFO) << "Registering fake keyboard";
    bool keyboard_registered = false;
    fuchsia::ui::test::input::RegistryRegisterKeyboardRequest request;
    request.set_device(fake_keyboard_.NewRequest());
    input_registry_->RegisterKeyboard(std::move(request),
                                      [&keyboard_registered]() { keyboard_registered = true; });
    RunLoopUntil([&keyboard_registered] { return keyboard_registered; });
    FX_LOGS(INFO) << "Keyboard registered";
  }

  // The touch screen is used to bring the input text area under test into
  // keyboard focus.
  void RegisterTouchScreen() {
    FX_CHECK(input_registry_.is_bound()) << "input_registry_ is not initialized";
    FX_LOGS(INFO) << "Registering fake touch screen";

    bool touch_screen_registered = false;
    fuchsia::ui::test::input::RegistryRegisterTouchScreenRequest request;
    request.set_device(fake_touch_screen_.NewRequest());
    input_registry_->RegisterTouchScreen(
        std::move(request), [&touch_screen_registered] { touch_screen_registered = true; });

    RunLoopUntil([&touch_screen_registered] { return touch_screen_registered; });
    FX_LOGS(INFO) << "Touch screen registered";
  }

  // Injects an on-screen tap at the given screen coordinates.
  void InjectTap(int32_t x, int32_t y) {
    const fuchsia::math::Vec location = {.x = x, .y = y};
    fuchsia::ui::test::input::TouchScreenSimulateTapRequest request;
    request.set_tap_location(location);

    fake_touch_screen_->SimulateTap(std::move(request), [this] {
      ++injection_count_;
      FX_LOGS(INFO) << "*** Tap injected, count: " << injection_count_;
    });
  }

  // Periodically taps the (x,y) coordinate on the screen.
  // Call CancelTap() to cancel the periodic tap task.
  void TryTapUntilCanceled(int32_t x, int32_t y) {
    InjectTap(x, y);
    inject_retry_task_.emplace(
        [this, x, y](auto dispatcher, auto task, auto status) { TryTapUntilCanceled(x, y); });
    FX_CHECK(inject_retry_task_->PostDelayed(dispatcher(), kTapRetryInterval) == ZX_OK);
  }

  void CancelTaps() {
    inject_retry_task_.reset();
    FX_LOGS(INFO) << "Taps canceled as our window is in focus";
  }

  void AssembleRealm(const std::vector<std::pair<ChildName, std::string>>& components,
                     const std::vector<Route>& routes) {
    FX_LOGS(INFO) << "Building realm";
    realm_ = ui_test_manager_->AddSubrealm();

    // Key part of service setup: have this test component vend the
    // |ResponseListener| service in the constructed realm.
    realm_->AddLocalChild(kResponseListener, [d = dispatcher(), s = keyboard_input_state_]() {
      return std::make_unique<KeyboardInputListenerServer>(d, s);
    });

    for (const auto& [name, component] : components) {
      realm_->AddChild(name, component);
    }

    // Add the necessary routing for each of the extra components added above.
    for (const auto& route : routes) {
      realm_->AddRoute(route);
    }

    // Finally, build the realm using the provided components and routes.
    ui_test_manager_->BuildRealm();
    realm_exposed_services_ = ui_test_manager_->CloneExposedServicesDirectory();
  }

  void LaunchClient() {
    // Initialize scene, and attach client view.
    ui_test_manager_->InitializeScene();
    FX_LOGS(INFO) << "Wait for client view to render";
    RunLoopUntil([this]() { return ui_test_manager_->ClientViewIsRendering(); });
  }

  // Guaranteed to be initialized after SetUp().
  uint32_t display_width() const {
    FX_CHECK(display_width_.has_value());
    return display_width_.value();
  }
  uint32_t display_height() const {
    FX_CHECK(display_height_.has_value());
    return display_height_.value();
  }

  std::optional<ui_testing::UITestManager> ui_test_manager_;
  std::unique_ptr<sys::ServiceDirectory> realm_exposed_services_;

  // Needs to outlive realm_ below.
  std::shared_ptr<KeyboardInputState> keyboard_input_state_;

  std::optional<Realm> realm_;

  // The registry for registering various fake devices.
  fuchsia::ui::test::input::RegistryPtr input_registry_;

  fuchsia::ui::test::input::KeyboardPtr fake_keyboard_;
  fuchsia::ui::test::input::TouchScreenPtr fake_touch_screen_;

  int injection_count_ = 0;
  std::optional<async::Task> inject_retry_task_;

 private:
  std::optional<uint32_t> display_width_ = std::nullopt;
  std::optional<uint32_t> display_height_ = std::nullopt;
};

class ChromiumInputTest : public ChromiumInputBase {
 protected:
  static constexpr auto kTextInputChromium = "text-input-chromium";
  static constexpr auto kTextInputChromiumUrl = "#meta/text-input-chromium.cm";

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

  static constexpr auto kFontsProvider = "fonts_provider";
  static constexpr auto kFontsProviderUrl = "#meta/font_provider_hermetic_for_test.cm";

  static constexpr auto kIntl = "intl";
  static constexpr auto kIntlUrl = "#meta/intl_property_manager.cm";

  std::vector<std::pair<ChildName, std::string>> GetTestComponents() override {
    return {
        std::make_pair(kTextInputChromium, kTextInputChromiumUrl),
        std::make_pair(kBuildInfoProvider, kBuildInfoProviderUrl),
        std::make_pair(kMemoryPressureProvider, kMemoryPressureProviderUrl),
        std::make_pair(kNetstack, kNetstackUrl),
        std::make_pair(kMockCobalt, kMockCobaltUrl),
        std::make_pair(kFontsProvider, kFontsProviderUrl),
        std::make_pair(kIntl, kIntlUrl),
        std::make_pair(kWebContextProvider, kWebContextProviderUrl),
    };
  }

  std::vector<Route> GetTestRoutes() override {
    return merge({GetChromiumRoutes(ChildRef{kTextInputChromium}),
                  {
                      {.capabilities = {Protocol{fuchsia::ui::app::ViewProvider::Name_}},
                       .source = ChildRef{kTextInputChromium},
                       .targets = {ParentRef()}},
                  }});
  }

  // Routes needed to setup Chromium client.
  static std::vector<Route> GetChromiumRoutes(ChildRef target) {
    return {
        {
            .capabilities =
                {
                    Protocol{fuchsia::logger::LogSink::Name_},
                },
            .source = ParentRef(),
            .targets =
                {
                    target, ChildRef{kFontsProvider}, ChildRef{kMemoryPressureProvider},
                    ChildRef{kBuildInfoProvider}, ChildRef{kWebContextProvider}, ChildRef{kIntl},
                    ChildRef{kMockCobalt},
                    // Not including kNetstack here, since it emits spurious
                    // FATAL errors.
                },
        },
        {.capabilities =
             {
                 Protocol{fuchsia::kernel::VmexResource::Name_},
                 Protocol{fuchsia::process::Launcher::Name_},
                 Protocol{fuchsia::ui::composition::Allocator::Name_},
                 Protocol{fuchsia::ui::composition::Flatland::Name_},
                 Protocol{fuchsia::vulkan::loader::Loader::Name_},
             },
         .source = ParentRef(),
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::fonts::Provider::Name_}},
         .source = ChildRef{kFontsProvider},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::tracing::provider::Registry::Name_},
                          Directory{.name = "config-data",
                                    .rights = fuchsia::io::R_STAR_DIR,
                                    .path = "/config/data"}},
         .source = ParentRef(),
         .targets = {ChildRef{kFontsProvider}, ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::intl::PropertyProvider::Name_}},
         .source = ChildRef{kIntl},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::ui::test::input::KeyboardInputListener::Name_}},
         .source = ChildRef{kResponseListener},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::memorypressure::Provider::Name_}},
         .source = ChildRef{kMemoryPressureProvider},
         .targets = {target}},

        {.capabilities = {Protocol{fuchsia::posix::socket::Provider::Name_},
                          Protocol{fuchsia::net::interfaces::State::Name_}},
         .source = ChildRef{kNetstack},
         // Use the .source below instead of above,
         // if you want to use Chrome remote debugging. See README.md for
         // instructions.
         //.source = ParentRef(),
         .targets = {target}},

        {.capabilities = {Protocol{fuchsia::web::ContextProvider::Name_}},
         .source = ChildRef{kWebContextProvider},
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::accessibility::semantics::SemanticsManager::Name_}},
         .source = ParentRef(),
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::metrics::MetricEventLoggerFactory::Name_}},
         .source = ChildRef{kMockCobalt},
         .targets = {ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::ui::input3::Keyboard::Name_}},
         .source = ParentRef(),
         .targets = {target}},
        {.capabilities = {Protocol{fuchsia::sysmem::Allocator::Name_}},
         .source = ParentRef(),
         .targets = {target, ChildRef{kMemoryPressureProvider}}},
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
         .targets = {target, ChildRef{kMemoryPressureProvider}}},
        {.capabilities = {Protocol{fuchsia::ui::scenic::Scenic::Name_}},
         .source = ParentRef(),
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

  void LaunchWebEngineClient() {
    LaunchClient();
    // Not quite exactly the location of the text area under test, but since the
    // text area occupies all the screen, it's very likely within the text area.
    TryTapUntilCanceled(display_width() / 2, display_height() / 2);
    FX_LOGS(INFO) << "Waiting on client view focused";
    RunLoopUntil([this] { return ui_test_manager_->ClientViewIsFocused(); });
    FX_LOGS(INFO) << "Waiting on response listener ready";
    RunLoopUntil([this]() { return keyboard_input_state_->IsReady(); });
    CancelTaps();
  }
};

// Launches a web engine to opens a page with a full-screen text input window.
// Then taps the screen to move focus to that page, and types text on the
// fake injected keyboard.  Loops around until the text appears in the
// text area.
TEST_F(ChromiumInputTest, BasicInputTest) {
  LaunchWebEngineClient();

  fuchsia::ui::test::input::KeyboardSimulateUsAsciiTextEntryRequest request;
  request.set_text("Hello\nworld!");

  // There is no need to wait for the text entry to finish, since the condition
  // below may only be fulfilled if it did, in fact, finish.
  fake_keyboard_->SimulateUsAsciiTextEntry(std::move(request), [] {});

  RunLoopUntil([&] { return keyboard_input_state_->ResponseContains("Hello\\nworld!"); });

  FX_LOGS(INFO) << "Done";
}

}  // namespace
