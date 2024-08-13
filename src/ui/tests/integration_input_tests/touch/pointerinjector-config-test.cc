// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.input.injection/cpp/fidl.h>
#include <fidl/fuchsia.ui.app/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.test.input/cpp/fidl.h>
#include <fidl/test.accessibility/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/channel.h>
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
#include <utility>

#include <gtest/gtest.h>
#include <src/ui/testing/util/fidl_cpp_helpers.h>
#include <src/ui/testing/util/portable_ui_test.h>

// This test exercises the pointer injector code in the context of Input Pipeline and a real Scenic
// client. It is a multi-component test, and carefully avoids sleeping or polling for component
// coordination.
// - It runs real Scene Manager and Scenic components.
// - It uses a fake display controller; the physical device is unused.
//
// Components involved
// - This test program
// - Scene Manager
// - Scenic
// - Child view, a Scenic client
//
// Touch dispatch path
// - Test program's injection -> Input Pipeline -> Scenic -> Child view
//
// Setup sequence
// - The test sets up this view hierarchy:
//   - Top level scene, owned by Scene Manager.
//   - Child view, owned by the ui client.
// - The test waits for a Scenic event that verifies the child has UI content in the scene graph.
// - The test injects input into Input Pipeline, emulating a display's touch report.
// - Input Pipeline dispatches the touch event to Scenic, which in turn dispatches it to the child.
// - The child receives the touch event and reports back to the test over a custom test-only FIDL.
// - Test waits for the child to report a touch; when the test receives the report, the test quits
//   successfully.
//
// This test uses the realm_builder library to construct the topology of components
// and routes services between them. For v2 components, every test driver component
// sits as a child of test_manager in the topology. Thus, the topology of a test
// driver component such as this one looks like this:
//
//     test_manager
//         |
//   pointerinjector-config-test.cml (this component)
//
// With the usage of the realm_builder library, we construct a realm during runtime
// and then extend the topology to look like:
//
//    test_manager
//         |
//   pointerinjector-config-test.cml (this component)
//         |
//   <created realm root>
//      /      \
//   scenic  scene_manager
//
// For more information about testing v2 components and realm_builder,
// visit the following links:
//
// Testing: https://fuchsia.dev/fuchsia-src/concepts/testing/v2
// Realm Builder: https://fuchsia.dev/fuchsia-src/development/components/v2/realm_builder

namespace {

// Types imported for the realm_builder library.
using component_testing::ChildRef;
using component_testing::LocalComponentImpl;
using component_testing::ParentRef;
using component_testing::Protocol;

// Alias for Component child name as provided to Realm Builder.
using ChildName = std::string;

// Alias for Component Legacy URL as provided to Realm Builder.
using LegacyUrl = std::string;

// Max timeout in failure cases.
// Set this as low as you can that still works across all test platforms.
constexpr zx::duration kTimeout = zx::min(5);

// Maximum distance between two view coordinates so that they are considered equal.
constexpr auto kViewCoordinateEpsilon = 0.01;

constexpr auto kMockResponseListener = "response_listener";

class ResponseState {
 public:
  using CallbackT =
      fit::function<void(fuchsia_ui_test_input::TouchInputListenerReportTouchInputRequest)>;
  void SetRespondCallback(CallbackT callback) { respond_callback_ = std::move(callback); }

 private:
  friend class ResponseListenerServer;
  CallbackT respond_callback_ = nullptr;
};

// This component implements fuchsia.ui.test.input.TouchInputListener
// and the interface for a RealmBuilder LocalComponent. A LocalComponent
// is a component that is implemented here in the test, as opposed to elsewhere
// in the system. When it's inserted to the realm, it will act like a proper
// component. This is accomplished, in part, because the realm_builder
// library creates the necessary plumbing. It creates a manifest for the component
// and routes all capabilities to and from it.
class ResponseListenerServer : public fidl::Server<fuchsia_ui_test_input::TouchInputListener>,
                               public LocalComponentImpl {
 public:
  explicit ResponseListenerServer(async_dispatcher_t* dispatcher,
                                  std::weak_ptr<ResponseState> state)
      : dispatcher_(dispatcher), state_(std::move(state)) {}

  // |fuchsia_ui_test_input::TouchInputListener|
  void ReportTouchInput(ReportTouchInputRequest& request,
                        ReportTouchInputCompleter::Sync& completer) override {
    FX_LOGS(INFO) << "ReportTouchInput";
    if (auto s = state_.lock()) {
      s->respond_callback_(std::move(request));
    }
  }

  // |LocalComponentImpl::Start|
  // When the component framework requests for this component to start, this
  // method will be invoked by the realm_builder library.
  void OnStart() override {
    // When this component starts, add a binding to the test.touch.ResponseListener
    // protocol to this component's outgoing directory.
    outgoing()->AddProtocol<fuchsia_ui_test_input::TouchInputListener>(
        bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  }

 private:
  async_dispatcher_t* dispatcher_ = nullptr;
  fidl::ServerBindingGroup<fuchsia_ui_test_input::TouchInputListener> bindings_;
  std::weak_ptr<ResponseState> state_;
};

struct PointerInjectorConfigTestData {
  int display_rotation;

  float clip_scale = 1.f;
  float clip_translation_x = 0.f;
  float clip_translation_y = 0.f;

  // expected location of the pointer event, in client view space, where the
  // range of the X and Y axes is [0, 1]
  float expected_x;
  float expected_y;
};

class PointerInjectorConfigTest
    : public ui_testing::PortableUITest,
      public testing::WithParamInterface<PointerInjectorConfigTestData> {
 protected:
  PointerInjectorConfigTest() = default;
  ~PointerInjectorConfigTest() override {
    FX_CHECK(touch_injection_request_count() > 0) << "injection expected but didn't happen.";
  }

  std::string GetTestUIStackUrl() override { return "#meta/test-ui-stack.cm"; }

  uint32_t display_rotation() override { return GetParam().display_rotation; }

  void SetUp() override {
    ui_testing::PortableUITest::SetUp();
    // Post a "just in case" quit task, if the test hangs.
    async::PostDelayedTask(
        dispatcher(),
        [] { FX_LOGS(FATAL) << "\n\n>> Test did not complete in time, terminating.  <<\n\n"; },
        kTimeout);

    // Get the display dimensions.
    FX_LOGS(INFO) << "Waiting for scenic display info";

    FX_LOGS(INFO) << "Got display_width = " << display_width()
                  << " and display_height = " << display_height();

    // Register input injection device.
    FX_LOGS(INFO) << "Registering input injection device";
    RegisterTouchScreen();

    // Launch client view, and wait until it's rendering to proceed with the test.
    LaunchClient();

    auto magnifier_connect = realm_root()->component().Connect<test_accessibility::Magnifier>();
    ZX_ASSERT_OK(magnifier_connect);
    fake_magnifier_ = fidl::SyncClient(std::move(magnifier_connect.value()));
  }

  // Waits for one or more pointer events; calls QuitLoop once one meets expectations.
  void WaitForAResponseMeetingExpectations(float expected_x, float expected_y,
                                           const std::string& component_name) {
    response_state()->SetRespondCallback(
        [this, expected_x, expected_y,
         component_name](fuchsia_ui_test_input::TouchInputListenerReportTouchInputRequest request) {
          FX_LOGS(INFO) << "Client received tap at (" << request.local_x().value() << ", "
                        << request.local_y().value() << ").";
          FX_LOGS(INFO) << "Expected tap is at approximately (" << expected_x << ", " << expected_y
                        << ").";

          // Allow for minor rounding differences in coordinates.
          EXPECT_EQ(request.component_name(), component_name);
          if (abs(request.local_x().value() - expected_x) <= kViewCoordinateEpsilon &&
              abs(request.local_y().value() - expected_y) <= kViewCoordinateEpsilon) {
            response_state()->SetRespondCallback([](const auto&) {});
            QuitLoop();
          }
        });
  }

  void TryInjectRepeatedly() {
    int32_t x = 0;
    int32_t y = 0;

    auto test_data = GetParam();

    switch (test_data.display_rotation) {
      case 0:
        x = display_width() / 4;
        y = display_height() / 4;
        break;
      case 90:
        // The /config/data/display_rotation (90) specifies how many degrees to rotate the
        // presentation child view, counter-clockwise, in a right-handed coordinate system. Thus,
        // the user observes the child view to rotate *clockwise* by that amount (90).
        x = 3 * display_width() / 4;
        y = display_height() / 4;
        break;
      default:
        FX_NOTREACHED();
    }

    InjectTapWithRetry(x, y);
  }

  void SetClipSpaceTransform(float scale, float x, float y, int display_rotation) {
    // HACK HACK HACK
    // TODO(https://fxbug.dev/42081619): Remove this when we move to the new gesture
    // disambiguation protocols.
    //
    // Because the FlatlandAcessibilityView::SetMagnificationTransform
    // hardcodes translation values at a display rotation of 270, this test
    // must normalize the values given the non-270 display rotations
    // config values.
    switch (display_rotation) {
      case 0:
        // Since a display rotation of 270 uses (x, y) as (y, -x), pass
        // in (y, -x), which will yield (x, y) to get an effective display
        // rotation of 0.
        ZX_ASSERT_OK(fake_magnifier_->SetMagnification({scale, y, -x}));
        break;
      case 90:
        // Since a display rotation of 270 uses (x, y) as (y, -x), pass
        // in (-x, -y), which will yield (-y, x) to get an effective display
        // rotation of 90.
        ZX_ASSERT_OK(fake_magnifier_->SetMagnification({scale, -x, -y}));
        break;
      default:
        FX_NOTREACHED();
    }
  }

  float display_width_float() { return static_cast<float>(display_width()); }
  float display_height_float() { return static_cast<float>(display_height()); }

  std::shared_ptr<ResponseState> response_state() { return response_state_; }

 private:
  void ExtendRealm() override {
    // Key part of service setup: have this test component vend the
    // |ResponseListener| service in the constructed realm.
    realm_builder().AddLocalChild(kMockResponseListener,
                                  [d = dispatcher(), s = response_state()]() {
                                    return std::make_unique<ResponseListenerServer>(d, s);
                                  });

    realm_builder().AddChild(kCppFlatlandClient, kCppFlatlandClientUrl);

    realm_builder().AddRoute(
        {.capabilities = {Protocol{fidl::DiscoverableProtocolName<fuchsia_ui_app::ViewProvider>}},
         .source = ChildRef{kCppFlatlandClient},
         .targets = {ParentRef()}});
    realm_builder().AddRoute(
        {.capabilities = {Protocol{
             fidl::DiscoverableProtocolName<fuchsia_ui_test_input::TouchInputListener>}},
         .source = ChildRef{kMockResponseListener},
         .targets = {ChildRef{kCppFlatlandClient}}});
    realm_builder().AddRoute(
        {.capabilities =
             {Protocol{fidl::DiscoverableProtocolName<fuchsia_ui_composition::Allocator>},
              Protocol{fidl::DiscoverableProtocolName<fuchsia_ui_composition::Flatland>}},
         .source = ui_testing::PortableUITest::kTestUIStackRef,
         .targets = {ChildRef{kCppFlatlandClient}}});
  }

  std::shared_ptr<ResponseState> response_state_ = std::make_shared<ResponseState>();

  fidl::SyncClient<test_accessibility::Magnifier> fake_magnifier_;

  static constexpr auto kCppFlatlandClient = "client";
  static constexpr auto kCppFlatlandClientUrl = "#meta/touch-flatland-client.cm";
};

// Declare test data.
// In all these tests, we tap the center of the top left quadrant of the
// physical display (after rotation), and verify that the client view gets a
// pointer event with the expected coordinates.

// No changes to display rotation or clip space
constexpr PointerInjectorConfigTestData kTestDataBaseCase = {
    .display_rotation = 0, .expected_x = 1.f / 4.f, .expected_y = 1.f / 4.f};

// Test scale by a factor of 2.
// Intuitive argument for these expected coordinates:
// Here we've zoomed into the center of the client view, scaling it up by 2x. So, the touch point
// will have 'migrated' halfway towards the center of the client view:  3/8 instead of 1/4.
constexpr PointerInjectorConfigTestData kTestDataScale = {
    .display_rotation = 0, .clip_scale = 2.f, .expected_x = 3.f / 8.f, .expected_y = 3.f / 8.f};

// Test display rotation by 90 degrees.
// In this case, rotation shouldn't affect what the client view sees.
constexpr PointerInjectorConfigTestData kTestDataRotateAndScale = {
    .display_rotation = 90, .clip_scale = 2.f, .expected_x = 3.f / 8.f, .expected_y = 3.f / 8.f};

// Test scaling and translation.
constexpr float kScale = 3.f;
constexpr float kTranslationX = -0.2f;
constexpr float kTranslationY = 0.1f;
constexpr PointerInjectorConfigTestData kTestDataScaleAndTranslate = {
    .display_rotation = 0,
    .clip_scale = kScale,
    .clip_translation_x = kTranslationX,
    .clip_translation_y = kTranslationY,
    // Terms: 'Original position' + 'movement due to scale' + 'movement due to translation'
    .expected_x = 0.25f + (0.25f * (1.f - 1.f / kScale)) - (kTranslationX / 2.f / kScale),
    .expected_y = 0.25f + (0.25f * (1.f - 1.f / kScale)) - (kTranslationY / 2.f / kScale)};

// Test scaling, translation, and rotation at once.
//
// Here, the translation does affect what the client view sees, so we have to account for it.
// This is what the translation looks like in client view coordinates, where it's rotated 90
// degrees.
constexpr float kClientViewTranslationX = kTranslationY;
constexpr float kClientViewTranslationY = -kTranslationX;
constexpr PointerInjectorConfigTestData kTestDataScaleTranslateRotate = {
    .display_rotation = 90,
    .clip_scale = kScale,
    .clip_translation_x = kTranslationX,
    .clip_translation_y = kTranslationY,
    // Same formula as before, but with different transform values.
    .expected_x = 0.25f + (0.25f * (1.f - 1.f / kScale)) - (kClientViewTranslationX / 2.f / kScale),
    .expected_y =
        0.25f + (0.25f * (1.f - 1.f / kScale)) - (kClientViewTranslationY / 2.f / kScale)};

INSTANTIATE_TEST_SUITE_P(PointerInjectorConfigTestWithParams, PointerInjectorConfigTest,
                         ::testing::Values(kTestDataBaseCase, kTestDataScale,
                                           kTestDataRotateAndScale, kTestDataScaleAndTranslate,
                                           kTestDataScaleTranslateRotate));

TEST_P(PointerInjectorConfigTest, CppClientTapTest) {
  auto test_data = GetParam();

  FX_LOGS(INFO) << "Starting test with params: display_rotation=" << test_data.display_rotation
                << ", clip_scale=" << test_data.clip_scale
                << ", clip_translation_x=" << test_data.clip_translation_x
                << ", clip_translation_y=" << test_data.clip_translation_y
                << ", expected_x=" << test_data.expected_x
                << ", expected_y=" << test_data.expected_y;

  SetClipSpaceTransform(test_data.clip_scale, test_data.clip_translation_x,
                        test_data.clip_translation_y, test_data.display_rotation);

  TryInjectRepeatedly();

  switch (test_data.display_rotation) {
    case 0:
      WaitForAResponseMeetingExpectations(
          /*expected_x=*/display_width_float() * test_data.expected_x,
          /*expected_y=*/display_height_float() * test_data.expected_y,
          /*component_name=*/"touch-flatland-client");
      break;
    case 90:
      WaitForAResponseMeetingExpectations(
          /*expected_x=*/display_height_float() * test_data.expected_x,
          /*expected_y=*/display_width_float() * test_data.expected_y,
          /*component_name=*/"touch-flatland-client");
      break;
    default:
      FX_NOTREACHED();
  }

  RunLoop();
}

}  // namespace
