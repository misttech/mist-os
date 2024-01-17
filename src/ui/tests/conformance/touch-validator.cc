// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/display/singleton/cpp/fidl.h>
#include <fuchsia/ui/input3/cpp/fidl.h>
#include <fuchsia/ui/pointer/cpp/fidl.h>
#include <fuchsia/ui/test/conformance/cpp/fidl.h>
#include <fuchsia/ui/test/input/cpp/fidl.h>
#include <fuchsia/ui/test/scene/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <zircon/errors.h>

#include <algorithm>
#include <memory>
#include <string>
#include <vector>

#include <gtest/gtest.h>

#include "src/ui/tests/conformance/conformance-test-base.h"

namespace ui_conformance_testing {

const std::string PUPPET_UNDER_TEST_FACTORY_SERVICE = "puppet-under-test-factory-service";
const std::string AUXILIARY_PUPPET_FACTORY_SERVICE = "auxiliary-puppet-factory-service";

// Maximum distance between two physical pixel coordinates so that they are considered equal.
constexpr double kEpsilon = 0.5f;

// TODO(https://fxbug.dev/125831): Two coordinates (x/y) systems can differ in scale (size of
// pixels).
void ExpectLocationAndPhase(
    const std::string& scoped_message,
    const fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest& e, double expected_x,
    double expected_y, fuchsia::ui::pointer::EventPhase expected_phase,
    const uint32_t expected_pointer_id) {
  SCOPED_TRACE(scoped_message);
  auto pixel_scale = e.has_device_pixel_ratio() ? e.device_pixel_ratio() : 1;
  auto actual_x = pixel_scale * e.local_x();
  auto actual_y = pixel_scale * e.local_y();
  EXPECT_NEAR(expected_x, actual_x, kEpsilon);
  EXPECT_NEAR(expected_y, actual_y, kEpsilon);
  EXPECT_EQ(expected_phase, e.phase());
  EXPECT_EQ(expected_pointer_id, e.pointer_id());
}

// Input stack does not guarantee the order of pointer in 1 contact report.
// So tests sort the request vector by pointer id and keep the order of
// event time.
void SortTouchInputRequestByTimeAndTimestampAndPointerId(
    std::vector<fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest>& requests) {
  std::sort(requests.begin(), requests.end(), [](const auto& a, const auto& b) {
    if (a.time_received() != b.time_received()) {
      return a.time_received() < b.time_received();
    }
    return a.pointer_id() < b.pointer_id();
  });
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

  std::vector<fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest>
  cloned_events_received() {
    auto res = std::vector<fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest>();
    for (auto& req : events_received_) {
      fuchsia::ui::test::input::TouchInputListenerReportTouchInputRequest clone;
      req.Clone(&clone);
      res.push_back(std::move(clone));
    }

    return res;
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
};

using device_pixel_ratio = float;

class TouchConformanceTest : public ui_conformance_test_base::ConformanceTest,
                             public ::testing::WithParamInterface<device_pixel_ratio> {
 public:
  ~TouchConformanceTest() override = default;

  void SetUp() override {
    ui_conformance_test_base::ConformanceTest::SetUp();

    // Register fake touch screen.
    {
      FX_LOGS(INFO) << "Connecting to input registry";
      auto input_registry = ConnectSyncIntoRealm<fuchsia::ui::test::input::Registry>();

      FX_LOGS(INFO) << "Registering fake touch screen";
      fuchsia::ui::test::input::RegistryRegisterTouchScreenRequest request;
      request.set_device(fake_touch_screen_.NewRequest());
      request.set_coordinate_unit(fuchsia::ui::test::input::CoordinateUnit::PHYSICAL_PIXELS);
      ASSERT_EQ(input_registry->RegisterTouchScreen(std::move(request)), ZX_OK);
    }

    // Get display dimensions.
    {
      FX_LOGS(INFO) << "Reading display dimensions";
      auto display_info = ConnectSyncIntoRealm<fuchsia::ui::display::singleton::Info>();

      fuchsia::ui::display::singleton::Metrics metrics;
      ASSERT_EQ(display_info->GetMetrics(&metrics), ZX_OK);

      display_width_ = metrics.extent_in_px().width;
      display_height_ = metrics.extent_in_px().height;

      FX_LOGS(INFO) << "Received display dimensions (" << display_width_ << ", " << display_height_
                    << ")";
    }
  }

 protected:
  int32_t display_width_as_int() { return static_cast<int32_t>(display_width_); }
  int32_t display_height_as_int() { return static_cast<int32_t>(display_height_); }

  fuchsia::ui::test::input::TouchScreenSyncPtr fake_touch_screen_;
  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
};

class SingleViewTouchConformanceTest : public TouchConformanceTest {
 public:
  ~SingleViewTouchConformanceTest() override = default;

  void SetUp() override {
    TouchConformanceTest::SetUp();

    fuchsia::ui::views::ViewCreationToken root_view_token;

    // Get root view token.
    {
      FX_LOGS(INFO) << "Creating root view token";

      auto controller = ConnectSyncIntoRealm<fuchsia::ui::test::scene::Controller>();

      fuchsia::ui::test::scene::ControllerPresentClientViewRequest req;
      auto [view_token, viewport_token] = scenic::ViewCreationTokenPair::New();
      req.set_viewport_creation_token(std::move(viewport_token));
      ASSERT_EQ(controller->PresentClientView(std::move(req)), ZX_OK);
      root_view_token = std::move(view_token);
    }

    auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
    auto keyboard = ConnectSyncIntoRealm<fuchsia::ui::input3::Keyboard>();
    {
      FX_LOGS(INFO) << "Create puppet under test";
      fuchsia::ui::test::conformance::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 PUPPET_UNDER_TEST_FACTORY_SERVICE),
                ZX_OK);

      fuchsia::ui::test::conformance::PuppetFactoryCreateResponse resp;

      puppet_ = std::make_unique<TouchPuppet>();

      fuchsia::ui::test::conformance::PuppetCreationArgs creation_args;
      creation_args.set_server_end(puppet_->puppet_ptr.NewRequest());
      creation_args.set_view_token(std::move(root_view_token));
      creation_args.set_touch_listener(puppet_->touch_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(GetParam());

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), fuchsia::ui::test::conformance::Result::SUCCESS);
    }
  }

 protected:
  std::unique_ptr<TouchPuppet> puppet_;
};

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, SingleViewTouchConformanceTest,
                         ::testing::Values(1.0, 2.0));

TEST_P(SingleViewTouchConformanceTest, SimpleTap) {
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
  ExpectLocationAndPhase("[0]", events_received[0], kTapX, kTapY,
                         fuchsia::ui::pointer::EventPhase::ADD, 1);
  ExpectLocationAndPhase("[1]", events_received[1], kTapX, kTapY,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 1);
}

// Send input report contains 3 contacts and then release all of them to
// simultate multi touch down on touchscreen. UI client expects 3 touch
// add events, and 3 touch remove events.
TEST_P(SingleViewTouchConformanceTest, MultiTap) {
  const int kTap1X = 3 * display_width_as_int() / 4;
  const int kTapY = display_height_as_int() / 4;
  const int kTap0X = kTap1X - 5;
  const int kTap2X = kTap1X + 5;

  fuchsia::ui::test::input::TouchScreenSimulateMultiTapRequest multi_tap_req;
  multi_tap_req.mutable_tap_locations()->push_back({.x = kTap0X, .y = kTapY});
  multi_tap_req.mutable_tap_locations()->push_back({.x = kTap1X, .y = kTapY});
  multi_tap_req.mutable_tap_locations()->push_back({.x = kTap2X, .y = kTapY});
  FX_LOGS(INFO) << "Injecting tap at (" << kTap0X << ", " << kTapY << ") (" << kTap1X << ", "
                << kTapY << ") (" << kTap2X << ", " << kTapY << ")";
  fake_touch_screen_->SimulateMultiTap(std::move(multi_tap_req));

  FX_LOGS(INFO) << "Waiting for touch event listener to receive response";

  // There are 3 * touch down and 3 * touch remove, totally 6 events.
  const size_t kExpectedEventCounts = 6;
  RunLoopUntil([this]() {
    return this->puppet_->touch_listener.events_received().size() >= kExpectedEventCounts;
  });

  auto events_received = this->puppet_->touch_listener.cloned_events_received();
  ASSERT_EQ(events_received.size(), kExpectedEventCounts);

  SortTouchInputRequestByTimeAndTimestampAndPointerId(events_received);

  ExpectLocationAndPhase("add [0]", events_received[0], kTap0X, kTapY,
                         fuchsia::ui::pointer::EventPhase::ADD, 0);
  ExpectLocationAndPhase("add [1]", events_received[1], kTap1X, kTapY,
                         fuchsia::ui::pointer::EventPhase::ADD, 1);
  ExpectLocationAndPhase("add [2]", events_received[2], kTap2X, kTapY,
                         fuchsia::ui::pointer::EventPhase::ADD, 2);

  ExpectLocationAndPhase("remove [0]", events_received[3], kTap0X, kTapY,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 0);
  ExpectLocationAndPhase("remove [1]", events_received[4], kTap1X, kTapY,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 1);
  ExpectLocationAndPhase("remove [2]", events_received[5], kTap2X, kTapY,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 2);
}

// 2 finger contact then move apart horizontally to simulate pinch zoom.
// UI Client expects touch add, touch change and touch remove for
// 2 fingers.
TEST_P(SingleViewTouchConformanceTest, Pinch) {
  const int kTapX = 3 * display_width_as_int() / 4;
  const int kTapY = display_height_as_int() / 4;

  fuchsia::ui::test::input::TouchScreenSimulateMultiFingerGestureRequest pinch_req;
  pinch_req.set_start_locations({fuchsia::math::Vec{.x = kTapX - 5, .y = kTapY},
                                 fuchsia::math::Vec{.x = kTapX + 5, .y = kTapY}});
  pinch_req.set_end_locations({fuchsia::math::Vec{.x = kTapX - 20, .y = kTapY},
                               fuchsia::math::Vec{.x = kTapX + 20, .y = kTapY}});
  pinch_req.set_move_event_count(3);
  pinch_req.set_finger_count(2);
  FX_LOGS(INFO) << "Injecting pinch from (" << pinch_req.start_locations()[0].x << ", "
                << pinch_req.start_locations()[0].y << ") (" << pinch_req.start_locations()[1].x
                << ", " << pinch_req.start_locations()[1].y << ") to ("
                << pinch_req.end_locations()[0].x << ", " << pinch_req.end_locations()[0].y << ") ("
                << pinch_req.end_locations()[1].x << ", " << pinch_req.end_locations()[1].y
                << ") with move_event_count = " << pinch_req.move_event_count();
  fake_touch_screen_->SimulateMultiFingerGesture(std::move(pinch_req));

  FX_LOGS(INFO) << "Waiting for touch event listener to receive response";

  // There are 2 * touch down + 4 * touch change + 2 * touch remove, totally 8 events.
  const size_t kExpectedEventCounts = 8;

  RunLoopUntil([this]() {
    return this->puppet_->touch_listener.events_received().size() >= kExpectedEventCounts;
  });

  auto events_received = this->puppet_->touch_listener.cloned_events_received();
  ASSERT_EQ(events_received.size(), kExpectedEventCounts);

  SortTouchInputRequestByTimeAndTimestampAndPointerId(events_received);

  ExpectLocationAndPhase("add [0]", events_received[0], kTapX - 5, kTapY,
                         fuchsia::ui::pointer::EventPhase::ADD, 0);
  ExpectLocationAndPhase("add [1]", events_received[1], kTapX + 5, kTapY,
                         fuchsia::ui::pointer::EventPhase::ADD, 1);

  ExpectLocationAndPhase("change [0] 1", events_received[2], kTapX - 10, kTapY,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 0);
  ExpectLocationAndPhase("change [1] 1", events_received[3], kTapX + 10, kTapY,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 1);

  ExpectLocationAndPhase("change [0] 2", events_received[4], kTapX - 15, kTapY,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 0);
  ExpectLocationAndPhase("change [1] 2", events_received[5], kTapX + 15, kTapY,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 1);

  ExpectLocationAndPhase("remove [1]", events_received[6], kTapX - 15, kTapY,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 0);
  ExpectLocationAndPhase("remove [2]", events_received[7], kTapX + 15, kTapY,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 1);
}

class EmbeddedViewTouchConformanceTest : public TouchConformanceTest {
 public:
  ~EmbeddedViewTouchConformanceTest() override = default;

  void SetUp() override {
    TouchConformanceTest::SetUp();

    fuchsia::ui::views::ViewCreationToken root_view_token;

    // Get root view token.
    {
      FX_LOGS(INFO) << "Creating root view token";
      auto controller = ConnectSyncIntoRealm<fuchsia::ui::test::scene::Controller>();

      fuchsia::ui::test::scene::ControllerPresentClientViewRequest req;
      auto [view_token, viewport_token] = scenic::ViewCreationTokenPair::New();
      req.set_viewport_creation_token(std::move(viewport_token));
      ASSERT_EQ(controller->PresentClientView(std::move(req)), ZX_OK);
      root_view_token = std::move(view_token);
    }

    {
      FX_LOGS(INFO) << "Create parent puppet";

      fuchsia::ui::test::conformance::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 AUXILIARY_PUPPET_FACTORY_SERVICE),
                ZX_OK);

      fuchsia::ui::test::conformance::PuppetFactoryCreateResponse resp;

      parent_puppet_ = std::make_unique<TouchPuppet>();

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fuchsia::ui::input3::Keyboard>();

      fuchsia::ui::test::conformance::PuppetCreationArgs creation_args;
      creation_args.set_server_end(parent_puppet_->puppet_ptr.NewRequest());
      creation_args.set_view_token(std::move(root_view_token));
      creation_args.set_touch_listener(parent_puppet_->touch_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(GetParam());

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), fuchsia::ui::test::conformance::Result::SUCCESS);
    }

    // Create child viewport.
    fuchsia::ui::test::conformance::PuppetEmbedRemoteViewResponse embed_remote_view_response;
    {
      FX_LOGS(INFO) << "Creating child viewport";
      const uint64_t kChildViewportId = 1u;
      fuchsia::ui::test::conformance::PuppetEmbedRemoteViewRequest embed_remote_view_request;
      embed_remote_view_request.set_id(kChildViewportId);
      embed_remote_view_request.mutable_properties()->mutable_bounds()->set_size(
          {.width = display_width_ / 2, .height = display_height_ / 2});
      embed_remote_view_request.mutable_properties()->mutable_bounds()->set_origin(
          {.x = display_width_as_int() / 2, .y = display_height_as_int() / 2});
      this->parent_puppet_->puppet_ptr->EmbedRemoteView(std::move(embed_remote_view_request),
                                                        &embed_remote_view_response);
    }

    // Create child view.
    {
      FX_LOGS(INFO) << "Creating child puppet";
      fuchsia::ui::test::conformance::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 PUPPET_UNDER_TEST_FACTORY_SERVICE),
                ZX_OK);

      fuchsia::ui::test::conformance::PuppetFactoryCreateResponse resp;

      child_puppet_ = std::make_unique<TouchPuppet>();

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fuchsia::ui::input3::Keyboard>();

      fuchsia::ui::test::conformance::PuppetCreationArgs creation_args;
      creation_args.set_server_end(child_puppet_->puppet_ptr.NewRequest());
      creation_args.set_view_token(
          std::move(*embed_remote_view_response.mutable_view_creation_token()));
      creation_args.set_touch_listener(child_puppet_->touch_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(GetParam());

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), fuchsia::ui::test::conformance::Result::SUCCESS);
    }
  }

 protected:
  std::unique_ptr<TouchPuppet> parent_puppet_;
  std::unique_ptr<TouchPuppet> child_puppet_;
};

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, EmbeddedViewTouchConformanceTest,
                         ::testing::Values(1.0, 2.0));

TEST_P(EmbeddedViewTouchConformanceTest, EmbeddedViewTap) {
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
  ExpectLocationAndPhase("[0]", events_received[0], quarter_display_width, quarter_display_height,
                         fuchsia::ui::pointer::EventPhase::ADD, 1);
  ExpectLocationAndPhase("[1]", events_received[1], quarter_display_width, quarter_display_height,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 1);

  // The parent should not have received any pointer events.
  EXPECT_TRUE(this->parent_puppet_->touch_listener.events_received().empty());
}

// Send input report contains 3 contacts and then release all of them to
// simultate multi touch down on touchscreen. Embedded view expects 3 touch
// add events, and 3 touch remove events. And no events on parenet view.
TEST_P(EmbeddedViewTouchConformanceTest, EmbeddedViewMultiTap) {
  const int kTap1X = 3 * display_width_as_int() / 4;
  const int kTapY = 3 * display_height_as_int() / 4;
  const int kTap0X = kTap1X - 5;
  const int kTap2X = kTap1X + 5;

  fuchsia::ui::test::input::TouchScreenSimulateMultiTapRequest multi_tap_req;
  multi_tap_req.mutable_tap_locations()->push_back({.x = kTap0X, .y = kTapY});
  multi_tap_req.mutable_tap_locations()->push_back({.x = kTap1X, .y = kTapY});
  multi_tap_req.mutable_tap_locations()->push_back({.x = kTap2X, .y = kTapY});
  FX_LOGS(INFO) << "Injecting tap at (" << kTap0X << ", " << kTapY << ") (" << kTap1X << ", "
                << kTapY << ") (" << kTap2X << ", " << kTapY << ")";
  fake_touch_screen_->SimulateMultiTap(std::move(multi_tap_req));

  FX_LOGS(INFO) << "Waiting for touch event listener to receive response";
  // There are 3 * touch down and 3 * touch remove, totally 6 events.
  const size_t kExpectedEventCounts = 6;

  RunLoopUntil([this]() {
    return this->child_puppet_->touch_listener.events_received().size() >= kExpectedEventCounts;
  });

  auto events_received = this->child_puppet_->touch_listener.cloned_events_received();
  ASSERT_EQ(events_received.size(), kExpectedEventCounts);

  const double kTap1XChildView = static_cast<double>(display_width_) / 4.f;
  const double kTapYChildView = static_cast<double>(display_height_) / 4.f;
  const double kTap0XChildView = kTap1XChildView - 5;
  const double kTap2XChildView = kTap1XChildView + 5;

  SortTouchInputRequestByTimeAndTimestampAndPointerId(events_received);

  ExpectLocationAndPhase("add [0]", events_received[0], kTap0XChildView, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::ADD, 0);
  ExpectLocationAndPhase("add [1]", events_received[1], kTap1XChildView, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::ADD, 1);
  ExpectLocationAndPhase("add [2]", events_received[2], kTap2XChildView, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::ADD, 2);

  ExpectLocationAndPhase("remove [1]", events_received[3], kTap0XChildView, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 0);
  ExpectLocationAndPhase("remove [2]", events_received[4], kTap1XChildView, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 1);
  ExpectLocationAndPhase("remove [3]", events_received[5], kTap2XChildView, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 2);

  // The parent should not have received any pointer events.
  EXPECT_TRUE(this->parent_puppet_->touch_listener.events_received().empty());
}

// 2 finger contact then move apart horizontally to simulate pinch zoom.
// Embededd view expects touch add, touch change and touch remove for
// 2 fingers.
TEST_P(EmbeddedViewTouchConformanceTest, Pinch) {
  const int kTapX = 3 * display_width_as_int() / 4;
  const int kTapY = 3 * display_height_as_int() / 4;

  fuchsia::ui::test::input::TouchScreenSimulateMultiFingerGestureRequest pinch_req;
  pinch_req.set_start_locations({fuchsia::math::Vec{.x = kTapX - 5, .y = kTapY},
                                 fuchsia::math::Vec{.x = kTapX + 5, .y = kTapY}});
  pinch_req.set_end_locations({fuchsia::math::Vec{.x = kTapX - 20, .y = kTapY},
                               fuchsia::math::Vec{.x = kTapX + 20, .y = kTapY}});
  pinch_req.set_move_event_count(3);
  pinch_req.set_finger_count(2);

  FX_LOGS(INFO) << "Injecting pinch from (" << pinch_req.start_locations()[0].x << ", "
                << pinch_req.start_locations()[0].y << ") (" << pinch_req.start_locations()[1].x
                << ", " << pinch_req.start_locations()[1].y << ") to ("
                << pinch_req.end_locations()[0].x << ", " << pinch_req.end_locations()[0].y << ") ("
                << pinch_req.end_locations()[1].x << ", " << pinch_req.end_locations()[1].y
                << ") with move_event_count = " << pinch_req.move_event_count();
  fake_touch_screen_->SimulateMultiFingerGesture(std::move(pinch_req));

  FX_LOGS(INFO) << "Waiting for touch event listener to receive response";

  // There are 2 * touch down + 4 * touch change + 2 * touch remove, totally 8 events.
  const size_t kExpectedEventCounts = 8;

  RunLoopUntil([this]() {
    return this->child_puppet_->touch_listener.events_received().size() >= kExpectedEventCounts;
  });

  auto events_received = this->child_puppet_->touch_listener.cloned_events_received();
  ASSERT_EQ(events_received.size(), kExpectedEventCounts);

  const int kTapXChildView = display_width_as_int() / 4;
  const int kTapYChildView = display_height_as_int() / 4;

  SortTouchInputRequestByTimeAndTimestampAndPointerId(events_received);

  ExpectLocationAndPhase("add [0]", events_received[0], kTapXChildView - 5, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::ADD, 0);
  ExpectLocationAndPhase("add [1]", events_received[1], kTapXChildView + 5, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::ADD, 1);

  ExpectLocationAndPhase("change [0] 1", events_received[2], kTapXChildView - 10, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 0);
  ExpectLocationAndPhase("change [1] 1", events_received[3], kTapXChildView + 10, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 1);

  ExpectLocationAndPhase("change [0] 2", events_received[4], kTapXChildView - 15, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 0);
  ExpectLocationAndPhase("change [1] 2", events_received[5], kTapXChildView + 15, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::CHANGE, 1);

  ExpectLocationAndPhase("remove [0]", events_received[6], kTapXChildView - 15, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 0);
  ExpectLocationAndPhase("remove [1]", events_received[7], kTapXChildView + 15, kTapYChildView,
                         fuchsia::ui::pointer::EventPhase::REMOVE, 1);

  // The parent should not have received any pointer events.
  EXPECT_TRUE(this->parent_puppet_->touch_listener.events_received().empty());
}

}  //  namespace ui_conformance_testing
