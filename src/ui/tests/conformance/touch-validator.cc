// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/test/conformance/cpp/fidl.h>
#include <fuchsia/ui/test/context/cpp/fidl.h>
#include <fuchsia/ui/test/input/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace ui_conformance_testing {

bool CompareDouble(double f0, double f1, double epsilon) { return std::abs(f0 - f1) <= epsilon; }

class TouchListener : public fuchsia::ui::test::input::TouchInputListener {
 public:
  TouchListener() : binding_(this) {}

  // |fuchsia::ui::test::input::TouchInputListener|
  void ReportTouchInput(
      fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest request) override {
    events_received_.push_back(std::move(request));
  }

  // Returns a client end bound to this object.
  fidl::InterfaceHandle<fuchsia::ui::test::input::TouchInputListener> NewBinding() {
    return binding_.NewBinding();
  }

  const std::vector<fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest>&
  events_received() {
    return events_received_;
  }

  bool LastEventReceivedMatches(float expected_x, float expected_y) {
    if (events_received_.empty()) {
      return false;
    }

    const auto& last_event = events_received_.back();

    auto pixel_scale = last_event.has_device_pixel_ratio() ? last_event.device_pixel_ratio() : 1;

    auto actual_x = pixel_scale * last_event.local_x();
    auto actual_y = pixel_scale * last_event.local_y();

    FX_LOGS(INFO) << "Expecting event at (" << expected_x << ", " << expected_y << ")";
    FX_LOGS(INFO) << "Received event at (" << actual_x << ", " << actual_y
                  << "), accounting for pixel scale of " << pixel_scale;

    return CompareDouble(actual_x, expected_x, pixel_scale) &&
           CompareDouble(actual_y, expected_y, pixel_scale);
  }

 private:
  fidl::Binding<fuchsia::ui::test::input::TouchInputListener> binding_;
  std::vector<fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest> events_received_;
};

// Holds resources associated with a single puppet instance.
struct TouchPuppet {
  fuchsia::ui::test::conformance::PuppetSyncPtr puppet_ptr;
  TouchListener touch_listener;

  TouchPuppet(fuchsia::ui::test::context::Context_Sync* context,
              fuchsia::ui::views::ViewCreationToken view_creation_token,
              bool is_puppet_under_test = false) {
    fuchsia::ui::test::conformance::PuppetCreationArgs puppet_creation_args;
    puppet_creation_args.set_server_end(puppet_ptr.NewRequest());
    puppet_creation_args.set_view_token(std::move(view_creation_token));
    puppet_creation_args.set_touch_listener(touch_listener.NewBinding());
    if (is_puppet_under_test) {
      context->ConnectToPuppetUnderTest(std::move(puppet_creation_args));
    } else {
      context->ConnectToAuxiliaryPuppet(std::move(puppet_creation_args));
    }
  }
};

class TouchConformanceTest : public gtest::RealLoopFixture {
 public:
  ~TouchConformanceTest() override = default;

  void SetUp() override {
    {
      auto component_context = sys::ComponentContext::Create();
      auto test_context_factory =
          component_context->svc()->Connect<fuchsia::ui::test::context::Factory>();
      fuchsia::ui::test::context::FactoryCreateRequest request;
      request.set_context_server(test_context_.NewRequest());
      test_context_factory->Create(std::move(request));
    }

    // Register fake touch screen.
    {
      FX_LOGS(INFO) << "Connecting to input registry";
      fuchsia::ui::test::input::RegistryPtr input_registry;
      test_context_->ConnectToInputRegistry(input_registry.NewRequest());

      FX_LOGS(INFO) << "Registering fake touch screen";
      fuchsia::ui::test::input::RegistryRegisterTouchScreenRequest request;
      request.set_device(fake_touch_screen_.NewRequest());
      input_registry->RegisterTouchScreen(std::move(request), [this]() { QuitLoop(); });
      RunLoop();
    }

    // Get display dimensions.
    {
      FX_LOGS(INFO) << "Reading display dimensions";
      fuchsia::ui::test::context::ContextGetDisplayDimensionsResponse response;
      test_context_->GetDisplayDimensions(&response);
      ASSERT_TRUE(response.has_width_in_physical_px());
      display_width_ = response.width_in_physical_px();
      ASSERT_TRUE(response.has_height_in_physical_px());
      display_height_ = response.height_in_physical_px();
      FX_LOGS(INFO) << "Received display dimensions (" << display_width_ << ", " << display_height_
                    << ")";
    }
  }

 protected:
  int32_t display_width_as_int() { return static_cast<int32_t>(display_width_); }
  int32_t display_height_as_int() { return static_cast<int32_t>(display_height_); }

  fuchsia::ui::test::context::ContextSyncPtr test_context_;
  fuchsia::ui::test::input::TouchScreenSyncPtr fake_touch_screen_;
  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
};

TEST_F(TouchConformanceTest, SimpleTap) {
  // Get root view token.
  //
  // Note that the test context will automatically present the view created
  // using this token to the scene.
  FX_LOGS(INFO) << "Creating root view token";
  fuchsia::ui::views::ViewCreationToken view_creation_token;
  test_context_->GetRootViewToken(&view_creation_token);

  // Create puppet using root view token.
  FX_LOGS(INFO) << "Creating puppet under test";
  TouchPuppet puppet(test_context_.get(), std::move(view_creation_token),
                     /* is_puppet_under_test = */ true);

  // Inject tap in the middle of the top-right quadrant.
  fuchsia::ui::test::input::TouchScreenSimulateTapRequest tap_request;
  tap_request.mutable_tap_location()->x = 500;
  tap_request.mutable_tap_location()->y = -500;
  FX_LOGS(INFO) << "Injecting tap at (500, -500)";
  fake_touch_screen_->SimulateTap(std::move(tap_request));

  FX_LOGS(INFO) << "Waiting for touch event listener to receive response";
  RunLoopUntil([this, &puppet]() {
    return puppet.touch_listener.LastEventReceivedMatches(
        static_cast<float>(display_width_) * 3.f / 4.f, static_cast<float>(display_height_) / 4.f);
  });
}

TEST_F(TouchConformanceTest, EmbeddedViewTap) {
  const uint64_t kChildViewportId = 1u;

  // Get root view token.
  //
  // Note that the test context will automatically present the view created
  // using this token to the scene.
  FX_LOGS(INFO) << "Creating root view token";
  fuchsia::ui::views::ViewCreationToken parent_view_creation_token;
  test_context_->GetRootViewToken(&parent_view_creation_token);

  // Create parent puppet using root view token.
  FX_LOGS(INFO) << "Creating parent puppet";
  TouchPuppet parent(test_context_.get(), std::move(parent_view_creation_token),
                     /* is_puppet_under_test = */ false);

  // Create child viewport.
  FX_LOGS(INFO) << "Creating child viewport";
  fuchsia::ui::test::conformance::PuppetEmbedRemoteViewRequest embed_remote_view_request;
  embed_remote_view_request.set_id(kChildViewportId);
  embed_remote_view_request.mutable_properties()->mutable_bounds()->set_size(
      {.width = display_width_ / 2, .height = display_height_ / 2});
  embed_remote_view_request.mutable_properties()->mutable_bounds()->set_origin(
      {.x = display_width_as_int() / 2, .y = display_height_as_int() / 2});
  fuchsia::ui::test::conformance::PuppetEmbedRemoteViewResponse embed_remote_view_response;
  parent.puppet_ptr->EmbedRemoteView(std::move(embed_remote_view_request),
                                     &embed_remote_view_response);

  // Create child view.
  FX_LOGS(INFO) << "Creating child puppet";
  TouchPuppet child(test_context_.get(),
                    std::move(*embed_remote_view_response.mutable_view_creation_token()),
                    /* is_puppet_under_test = */ true);

  // Inject tap in the middle of the bottom-right quadrant.
  fuchsia::ui::test::input::TouchScreenSimulateTapRequest tap_request;
  tap_request.mutable_tap_location()->x = 500;
  tap_request.mutable_tap_location()->y = 500;
  FX_LOGS(INFO) << "Injecting tap at (500, 500)";
  fake_touch_screen_->SimulateTap(std::move(tap_request));

  FX_LOGS(INFO) << "Waiting for child touch event listener to receive response";
  RunLoopUntil([this, &child]() {
    // The child view's origin is in the center of the screen, so the center of
    // the bottom-right quadrant is at (display_width_ / 4, display_height_ / 4)
    // in the child's local coordinate space.
    return child.touch_listener.LastEventReceivedMatches(static_cast<float>(display_width_) / 4.f,
                                                         static_cast<float>(display_height_) / 4.f);
  });

  // The parent should not have received any pointer events.
  EXPECT_TRUE(parent.touch_listener.events_received().empty());
}

}  //  namespace ui_conformance_testing
