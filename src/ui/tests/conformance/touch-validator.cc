// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/pointer/cpp/fidl.h>
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

// Maximum distance between two physical pixel coordinates so that they are considered equal.
constexpr double kEpsilon = 0.5f;

// TODO(fxbug.dev/125831): Two coordinates (x/y) systems can differ in scale (size of pixels).
void ExpectLocationAndPhase(
    const fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest& e, double expected_x,
    double expected_y, fuchsia::ui::pointer::EventPhase expected_phase) {
  auto pixel_scale = e.has_device_pixel_ratio() ? e.device_pixel_ratio() : 1;
  auto actual_x = pixel_scale * e.local_x();
  auto actual_y = pixel_scale * e.local_y();
  EXPECT_NEAR(expected_x, actual_x, kEpsilon);
  EXPECT_NEAR(expected_y, actual_y, kEpsilon);
  EXPECT_EQ(expected_phase, e.phase());
}

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

  bool LastEventReceivedMatchesPhase(fuchsia::ui::pointer::EventPhase phase) {
    if (events_received_.empty()) {
      return false;
    }

    const auto& last_event = events_received_.back();
    const auto actual_phase = last_event.phase();

    FX_LOGS(INFO) << "Expecting event at phase (" << static_cast<uint32_t>(phase) << ")";
    FX_LOGS(INFO) << "Received event at phase (" << static_cast<uint32_t>(actual_phase) << ")";

    return phase == actual_phase;
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
      request.set_coordinate_unit(fuchsia::ui::test::input::CoordinateUnit::PHYSICAL_PIXELS);
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

class SingleViewTouchConformanceTest : public TouchConformanceTest {
 public:
  ~SingleViewTouchConformanceTest() override = default;

  void SetUp() override {
    TouchConformanceTest::SetUp();

    // Get root view token.
    //
    // Note that the test context will automatically present the view created
    // using this token to the scene.
    FX_LOGS(INFO) << "Creating root view token";
    fuchsia::ui::views::ViewCreationToken view_creation_token;
    test_context_->GetRootViewToken(&view_creation_token);

    // Create puppet using root view token.
    FX_LOGS(INFO) << "Creating puppet under test";
    puppet_ = std::make_unique<TouchPuppet>(test_context_.get(), std::move(view_creation_token),
                                            /* is_puppet_under_test = */ true);
  }

 protected:
  std::unique_ptr<TouchPuppet> puppet_;
};

TEST_F(SingleViewTouchConformanceTest, SimpleTap) {
  const auto kTapX = 3 * display_width_as_int() / 4;
  const auto kTapY = display_height_as_int() / 4;

  // Inject tap in the middle of the top-right quadrant.
  fuchsia::ui::test::input::TouchScreenSimulateTapRequest tap_request;
  tap_request.mutable_tap_location()->x = kTapX;
  tap_request.mutable_tap_location()->y = kTapY;
  FX_LOGS(INFO) << "Injecting tap at (" << kTapX << ", " << kTapY << ")";
  fake_touch_screen_->SimulateTap(std::move(tap_request));

  FX_LOGS(INFO) << "Waiting for touch event listener to receive response";
  RunLoopUntil([this]() {
    return this->puppet_->touch_listener.LastEventReceivedMatchesPhase(
        fuchsia::ui::pointer::EventPhase::REMOVE);
  });

  const auto& events_received = this->puppet_->touch_listener.events_received();
  ASSERT_EQ(events_received.size(), 2u);
  // The puppet's view matches the display dimensions exactly, and the device
  // pixel ratio is 1. Therefore, the puppet's logical coordinate space will
  // match the physical coordinate space, so we expect the puppet to report
  // the event at (kTapX, kTapY).
  ExpectLocationAndPhase(events_received[0], kTapX, kTapY, fuchsia::ui::pointer::EventPhase::ADD);
  ExpectLocationAndPhase(events_received[1], kTapX, kTapY,
                         fuchsia::ui::pointer::EventPhase::REMOVE);
}

class EmbeddedViewTouchConformanceTest : public TouchConformanceTest {
 public:
  ~EmbeddedViewTouchConformanceTest() override = default;

  void SetUp() override {
    TouchConformanceTest::SetUp();
    // Get root view token.
    //
    // Note that the test context will automatically present the view created
    // using this token to the scene.
    FX_LOGS(INFO) << "Creating root view token";
    fuchsia::ui::views::ViewCreationToken parent_view_creation_token;
    test_context_->GetRootViewToken(&parent_view_creation_token);

    // Create parent puppet using root view token.
    FX_LOGS(INFO) << "Creating parent puppet";
    this->parent_puppet_ =
        std::make_unique<TouchPuppet>(test_context_.get(), std::move(parent_view_creation_token),
                                      /* is_puppet_under_test = */ false);

    // Create child viewport.
    FX_LOGS(INFO) << "Creating child viewport";
    fuchsia::ui::test::conformance::PuppetEmbedRemoteViewRequest embed_remote_view_request;
    const uint64_t kChildViewportId = 1u;
    embed_remote_view_request.set_id(kChildViewportId);
    embed_remote_view_request.mutable_properties()->mutable_bounds()->set_size(
        {.width = display_width_ / 2, .height = display_height_ / 2});
    embed_remote_view_request.mutable_properties()->mutable_bounds()->set_origin(
        {.x = display_width_as_int() / 2, .y = display_height_as_int() / 2});
    fuchsia::ui::test::conformance::PuppetEmbedRemoteViewResponse embed_remote_view_response;
    this->parent_puppet_->puppet_ptr->EmbedRemoteView(std::move(embed_remote_view_request),
                                                      &embed_remote_view_response);

    // Create child view.
    FX_LOGS(INFO) << "Creating child puppet";
    this->child_puppet_ = std::make_unique<TouchPuppet>(
        test_context_.get(), std::move(*embed_remote_view_response.mutable_view_creation_token()),
        /* is_puppet_under_test = */ true);
  }

 protected:
  std::unique_ptr<TouchPuppet> parent_puppet_;
  std::unique_ptr<TouchPuppet> child_puppet_;
};

TEST_F(EmbeddedViewTouchConformanceTest, EmbeddedViewTap) {
  const auto kTapX = 3 * display_width_as_int() / 4;
  const auto kTapY = 3 * display_height_as_int() / 4;

  // Inject tap in the middle of the bottom-right quadrant.
  fuchsia::ui::test::input::TouchScreenSimulateTapRequest tap_request;
  tap_request.mutable_tap_location()->x = kTapX;
  tap_request.mutable_tap_location()->y = kTapY;
  FX_LOGS(INFO) << "Injecting tap at (" << kTapX << ", " << kTapY << ")";
  fake_touch_screen_->SimulateTap(std::move(tap_request));

  FX_LOGS(INFO) << "Waiting for child touch event listener to receive response";

  RunLoopUntil([this]() {
    return this->child_puppet_->touch_listener.LastEventReceivedMatchesPhase(
        fuchsia::ui::pointer::EventPhase::REMOVE);
  });

  auto& events_received = this->child_puppet_->touch_listener.events_received();
  ASSERT_EQ(events_received.size(), 2u);

  // The child view's origin is in the center of the screen, so the center of
  // the bottom-right quadrant is at (display_width_ / 4, display_height_ / 4)
  // in the child's local coordinate space.
  const double quarter_display_width = static_cast<double>(display_width_) / 4.f;
  const double quarter_display_height = static_cast<double>(display_height_) / 4.f;
  ExpectLocationAndPhase(events_received[0], quarter_display_width, quarter_display_height,
                         fuchsia::ui::pointer::EventPhase::ADD);
  ExpectLocationAndPhase(events_received[1], quarter_display_width, quarter_display_height,
                         fuchsia::ui::pointer::EventPhase::REMOVE);

  // The parent should not have received any pointer events.
  EXPECT_TRUE(this->parent_puppet_->touch_listener.events_received().empty());
}

}  //  namespace ui_conformance_testing
