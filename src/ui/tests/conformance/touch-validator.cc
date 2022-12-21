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
  fuchsia::ui::test::context::ContextSyncPtr test_context_;
  fuchsia::ui::test::input::TouchScreenSyncPtr fake_touch_screen_;
  TouchListener touch_listener_;
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
  fuchsia::ui::test::conformance::PuppetPtr puppet_under_test;
  fuchsia::ui::test::conformance::PuppetCreationArgs puppet_creation_args;
  puppet_creation_args.set_server_end(puppet_under_test.NewRequest());
  puppet_creation_args.set_view_token(std::move(view_creation_token));
  puppet_creation_args.set_touch_listener(touch_listener_.NewBinding());
  test_context_->ConnectToPuppetUnderTest(std::move(puppet_creation_args));

  // Inject tap in the middle of the top-right quadrant.
  fuchsia::ui::test::input::TouchScreenSimulateTapRequest tap_request;
  tap_request.mutable_tap_location()->x = 500;
  tap_request.mutable_tap_location()->y = -500;
  FX_LOGS(INFO) << "Injecting tap at (500, -500)";
  fake_touch_screen_->SimulateTap(std::move(tap_request));

  FX_LOGS(INFO) << "Waiting for touch event listener to receive response";
  RunLoopUntil([this]() {
    return touch_listener_.LastEventReceivedMatches(static_cast<float>(display_width_) * 3.f / 4.f,
                                                    static_cast<float>(display_height_) / 4.f);
  });
}

}  //  namespace ui_conformance_testing
