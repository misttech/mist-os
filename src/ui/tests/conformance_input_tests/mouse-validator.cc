// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/display/singleton/cpp/fidl.h>
#include <fuchsia/ui/test/conformance/cpp/fidl.h>
#include <fuchsia/ui/test/input/cpp/fidl.h>
#include <fuchsia/ui/test/scene/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <zircon/errors.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/ui/tests/conformance_input_tests/conformance-test-base.h"

namespace ui_conformance_testing {

namespace futi = fuchsia::ui::test::input;
namespace futc = fuchsia::ui::test::conformance;

const std::string PUPPET_UNDER_TEST_FACTORY_SERVICE = "puppet-under-test-factory-service";

// Two physical pixel coordinates are considered equivalent if their distance is less than 1 unit.
constexpr double kPixelEpsilon = 0.5f;

// Epsilon for floating error.
constexpr double kEpsilon = 0.0001f;

class MouseListener : public futi::MouseInputListener {
 public:
  MouseListener() : binding_(this) {}

  // |fuchsia::ui::test::input::MouseInputListener|
  void ReportMouseInput(futi::MouseInputListenerReportMouseInputRequest request) override {
    events_received_.push_back(std::move(request));
  }

  // Returns a client end bound to this object.
  fidl::InterfaceHandle<futi::MouseInputListener> NewBinding() { return binding_.NewBinding(); }

  const std::vector<futi::MouseInputListenerReportMouseInputRequest>& events_received() const {
    return events_received_;
  }

  void clear_events() { events_received_.clear(); }

 private:
  fidl::Binding<futi::MouseInputListener> binding_;
  std::vector<futi::MouseInputListenerReportMouseInputRequest> events_received_;
};

// Holds resources associated with a single puppet instance.
struct MousePuppet {
  futc::PuppetSyncPtr puppet_ptr;
  MouseListener mouse_listener;
};

using device_pixel_ratio = float;

class MouseConformanceTest : public ui_conformance_test_base::ConformanceTest,
                             public ::testing::WithParamInterface<device_pixel_ratio> {
 public:
  ~MouseConformanceTest() override = default;

  float DevicePixelRatio() const override { return GetParam(); }

  void SetUp() override {
    ui_conformance_test_base::ConformanceTest::SetUp();

    // Register fake mouse.
    {
      FX_LOGS(INFO) << "Connecting to input registry";
      auto input_registry = ConnectSyncIntoRealm<futi::Registry>();

      FX_LOGS(INFO) << "Registering fake mouse";
      futi::RegistryRegisterMouseRequest request;
      request.set_device(fake_mouse_.NewRequest());
      ASSERT_EQ(input_registry->RegisterMouse(std::move(request)), ZX_OK);
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
      FX_LOGS(INFO) << "Create puppet under test";
      futc::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 PUPPET_UNDER_TEST_FACTORY_SERVICE),
                ZX_OK);

      futc::PuppetFactoryCreateResponse resp;

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fuchsia::ui::input3::Keyboard>();

      futc::PuppetCreationArgs creation_args;
      creation_args.set_server_end(puppet_.puppet_ptr.NewRequest());
      creation_args.set_view_token(std::move(root_view_token));
      creation_args.set_mouse_listener(puppet_.mouse_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(DevicePixelRatio());

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), futc::Result::SUCCESS);
    }
  }

  void SimulateMouseEvent(std::vector<futi::MouseButton> pressed_buttons, int movement_x,
                          int movement_y) {
    FX_LOGS(INFO) << "Requesting mouse event";
    futi::MouseSimulateMouseEventRequest request;
    request.set_pressed_buttons(std::move(pressed_buttons));
    request.set_movement_x(movement_x);
    request.set_movement_y(movement_y);

    fake_mouse_->SimulateMouseEvent(std::move(request));
    FX_LOGS(INFO) << "Mouse event injected";
  }

  void ExpectEvent(const std::string& scoped_message,
                   const futi::MouseInputListenerReportMouseInputRequest& e, double expected_x,
                   double expected_y,
                   const std::vector<futi::MouseButton>& expected_buttons) const {
    SCOPED_TRACE(scoped_message);
    auto pixel_scale = e.has_device_pixel_ratio() ? e.device_pixel_ratio() : 1;
    EXPECT_NEAR(static_cast<double>(DevicePixelRatio()), pixel_scale, kEpsilon);
    auto actual_x = pixel_scale * e.local_x();
    auto actual_y = pixel_scale * e.local_y();
    EXPECT_NEAR(expected_x, actual_x, kPixelEpsilon);
    EXPECT_NEAR(expected_y, actual_y, kPixelEpsilon);
    if (expected_buttons.empty()) {
      EXPECT_FALSE(e.has_buttons());
    } else {
      ASSERT_TRUE(e.has_buttons());
      EXPECT_EQ(expected_buttons, e.buttons());
    }
  }

 protected:
  int32_t display_width_as_int() const { return static_cast<int32_t>(display_width_); }
  int32_t display_height_as_int() const { return static_cast<int32_t>(display_height_); }

  futi::MouseSyncPtr fake_mouse_;
  MousePuppet puppet_;
  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
};

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, MouseConformanceTest, ::testing::Values(1.0, 2.0));

TEST_P(MouseConformanceTest, SimpleClick) {
  // Inject click with no mouse movement.
  // Left button down.
  SimulateMouseEvent(/* pressed_buttons = */ {futi::MouseButton::FIRST},
                     /* movement_x = */ 0, /* movement_y = */ 0);
  FX_LOGS(INFO) << "Waiting for puppet to report DOWN event";
  RunLoopUntil([this]() { return this->puppet_.mouse_listener.events_received().size() >= 1; });

  ASSERT_EQ(puppet_.mouse_listener.events_received().size(), 1u);

  ExpectEvent("Down", puppet_.mouse_listener.events_received()[0],
              /* expected_x = */ static_cast<double>(display_width_) / 2.f,
              /* expected_y = */ static_cast<double>(display_height_) / 2.f,
              /* expected_buttons = */ {futi::MouseButton::FIRST});

  puppet_.mouse_listener.clear_events();

  // Left button up.
  SimulateMouseEvent(/* pressed_buttons = */ {}, /* movement_x = */ 0, /* movement_y = */ 0);

  FX_LOGS(INFO) << "Waiting for puppet to report UP";
  RunLoopUntil([this]() { return this->puppet_.mouse_listener.events_received().size() >= 1; });
  ASSERT_EQ(puppet_.mouse_listener.events_received().size(), 1u);

  ExpectEvent("Up", puppet_.mouse_listener.events_received()[0],
              /* expected_x = */ static_cast<double>(display_width_) / 2.f,
              /* expected_y = */ static_cast<double>(display_height_) / 2.f,
              /* expected_buttons = */ {});
}

}  //  namespace ui_conformance_testing
