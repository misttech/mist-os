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

#include "src/ui/tests/conformance/conformance-test-base.h"

namespace ui_conformance_testing {

const std::string PUPPET_UNDER_TEST_FACTORY_SERVICE = "puppet-under-test-factory-service";

bool CompareDouble(double f0, double f1, double epsilon) { return std::abs(f0 - f1) <= epsilon; }

class MouseListener : public fuchsia::ui::test::input::MouseInputListener {
 public:
  MouseListener() : binding_(this) {}

  // |fuchsia::ui::test::input::MouseInputListener|
  void ReportMouseInput(
      fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest request) override {
    events_received_.push_back(std::move(request));
  }

  // Returns a client end bound to this object.
  fidl::InterfaceHandle<fuchsia::ui::test::input::MouseInputListener> NewBinding() {
    return binding_.NewBinding();
  }

  const std::vector<fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest>&
  events_received() const {
    return events_received_;
  }

 private:
  fidl::Binding<fuchsia::ui::test::input::MouseInputListener> binding_;
  std::vector<fuchsia::ui::test::input::MouseInputListenerReportMouseInputRequest> events_received_;
};

// Holds resources associated with a single puppet instance.
struct MousePuppet {
  fuchsia::ui::test::conformance::PuppetSyncPtr puppet_ptr;
  MouseListener mouse_listener;
};

class MouseConformanceTest : public ui_conformance_test_base::ConformanceTest {
 public:
  ~MouseConformanceTest() override = default;

  void SetUp() override {
    ui_conformance_test_base::ConformanceTest::SetUp();

    // Register fake mouse.
    {
      FX_LOGS(INFO) << "Connecting to input registry";
      auto input_registry = ConnectSyncIntoRealm<fuchsia::ui::test::input::Registry>();

      FX_LOGS(INFO) << "Registering fake mouse";
      fuchsia::ui::test::input::RegistryRegisterMouseRequest request;
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
      fuchsia::ui::test::conformance::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 PUPPET_UNDER_TEST_FACTORY_SERVICE),
                ZX_OK);

      fuchsia::ui::test::conformance::PuppetFactoryCreateResponse resp;

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fuchsia::ui::input3::Keyboard>();

      fuchsia::ui::test::conformance::PuppetCreationArgs creation_args;
      creation_args.set_server_end(puppet_.puppet_ptr.NewRequest());
      creation_args.set_view_token(std::move(root_view_token));
      creation_args.set_mouse_listener(puppet_.mouse_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), fuchsia::ui::test::conformance::Result::SUCCESS);
    }
  }

  void SimulateMouseEvent(std::vector<fuchsia::ui::test::input::MouseButton> pressed_buttons,
                          int movement_x, int movement_y) {
    FX_LOGS(INFO) << "Requesting mouse event";
    fuchsia::ui::test::input::MouseSimulateMouseEventRequest request;
    request.set_pressed_buttons(std::move(pressed_buttons));
    request.set_movement_x(movement_x);
    request.set_movement_y(movement_y);

    fake_mouse_->SimulateMouseEvent(std::move(request));
    FX_LOGS(INFO) << "Mouse event injected";
  }

  void WaitForEvent(const MousePuppet& puppet, double expected_x, double expected_y,
                    std::vector<fuchsia::ui::test::input::MouseButton> expected_buttons) {
    RunLoopUntil([&puppet, expected_x, expected_y, &expected_buttons] {
      const auto& events_received = puppet.mouse_listener.events_received();
      for (const auto& event : events_received) {
        const auto dpr = event.device_pixel_ratio();
        // Check position of event against expectation.
        if (!CompareDouble(event.local_x() * dpr, expected_x, dpr) ||
            !CompareDouble(event.local_y() * dpr, expected_y, dpr)) {
          continue;
        }

        // Check the reported buttons against expectations.
        //
        // For client ergonomics, we accept `buttons` as unset or set to the
        // empty vector if no buttons are pressed.
        if ((event.has_buttons() && event.buttons() == expected_buttons) ||
            (!event.has_buttons() && expected_buttons.empty())) {
          return true;
        }
      }

      return false;
    });
  }

 protected:
  int32_t display_width_as_int() { return static_cast<int32_t>(display_width_); }
  int32_t display_height_as_int() { return static_cast<int32_t>(display_height_); }

  fuchsia::ui::test::input::MouseSyncPtr fake_mouse_;
  MousePuppet puppet_;
  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
};

TEST_F(MouseConformanceTest, SimpleClick) {
  // Inject click with no mouse movement.
  // Left button down.
  SimulateMouseEvent(/* pressed_buttons = */ {fuchsia::ui::test::input::MouseButton::FIRST},
                     /* movement_x = */ 0, /* movement_y = */ 0);
  FX_LOGS(INFO) << "Waiting for puppet to report DOWN event";
  WaitForEvent(puppet_, /* expected_x = */ static_cast<double>(display_width_) / 2.f,
               /* expected_y = */ static_cast<double>(display_height_) / 2.f,
               /* expected_buttons = */ {fuchsia::ui::test::input::MouseButton::FIRST});

  // Left button up.
  SimulateMouseEvent(/* pressed_buttons = */ {}, /* movement_x = */ 0, /* movement_y = */ 0);

  FX_LOGS(INFO) << "Waiting for puppet to report UP";
  WaitForEvent(puppet_, /* expected_x = */ static_cast<double>(display_width_) / 2.f,
               /* expected_y = */ static_cast<double>(display_height_) / 2.f,
               /* expected_buttons = */ {});
}

}  //  namespace ui_conformance_testing
