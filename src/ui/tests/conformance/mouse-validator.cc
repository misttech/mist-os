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

  MousePuppet(fuchsia::ui::test::context::Context_Sync* context,
              fuchsia::ui::views::ViewCreationToken view_creation_token,
              bool is_puppet_under_test = false) {
    fuchsia::ui::test::conformance::PuppetCreationArgs puppet_creation_args;
    puppet_creation_args.set_server_end(puppet_ptr.NewRequest());
    puppet_creation_args.set_view_token(std::move(view_creation_token));
    puppet_creation_args.set_mouse_listener(mouse_listener.NewBinding());
    if (is_puppet_under_test) {
      context->ConnectToPuppetUnderTest(std::move(puppet_creation_args));
    } else {
      context->ConnectToAuxiliaryPuppet(std::move(puppet_creation_args));
    }
  }
};

class MouseConformanceTest : public gtest::RealLoopFixture {
 public:
  ~MouseConformanceTest() override = default;

  void SetUp() override {
    {
      auto component_context = sys::ComponentContext::Create();
      auto test_context_factory =
          component_context->svc()->Connect<fuchsia::ui::test::context::Factory>();
      fuchsia::ui::test::context::FactoryCreateRequest request;
      request.set_context_server(test_context_.NewRequest());
      test_context_factory->Create(std::move(request));
    }

    // Register fake mouse.
    {
      FX_LOGS(INFO) << "Connecting to input registry";
      fuchsia::ui::test::input::RegistrySyncPtr input_registry;
      test_context_->ConnectToInputRegistry(input_registry.NewRequest());

      FX_LOGS(INFO) << "Registering fake mouse";
      fuchsia::ui::test::input::RegistryRegisterMouseRequest request;
      request.set_device(fake_mouse_.NewRequest());
      input_registry->RegisterMouse(std::move(request));
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

  fuchsia::ui::test::context::ContextSyncPtr test_context_;
  fuchsia::ui::test::input::MouseSyncPtr fake_mouse_;
  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
};

TEST_F(MouseConformanceTest, SimpleClick) {
  // Get root view token.
  //
  // Note that the test context will automatically present the view created
  // using this token to the scene.
  FX_LOGS(INFO) << "Creating root view token";
  fuchsia::ui::views::ViewCreationToken view_creation_token;
  test_context_->GetRootViewToken(&view_creation_token);

  // Create puppet using root view token.
  FX_LOGS(INFO) << "Creating puppet under test";
  MousePuppet puppet(test_context_.get(), std::move(view_creation_token),
                     /* is_puppet_under_test = */ true);

  // Inject click with no mouse movement.
  // Left button down.
  SimulateMouseEvent(/* pressed_buttons = */ {fuchsia::ui::test::input::MouseButton::FIRST},
                     /* movement_x = */ 0, /* movement_y = */ 0);
  FX_LOGS(INFO) << "Waiting for puppet to report DOWN event";
  WaitForEvent(puppet, /* expected_x = */ static_cast<double>(display_width_) / 2.f,
               /* expected_y = */ static_cast<double>(display_height_) / 2.f,
               /* expected_buttons = */ {fuchsia::ui::test::input::MouseButton::FIRST});

  // Left button up.
  SimulateMouseEvent(/* pressed_buttons = */ {}, /* movement_x = */ 0, /* movement_y = */ 0);

  FX_LOGS(INFO) << "Waiting for puppet to report UP";
  WaitForEvent(puppet, /* expected_x = */ static_cast<double>(display_width_) / 2.f,
               /* expected_y = */ static_cast<double>(display_height_) / 2.f,
               /* expected_buttons = */ {});
}

}  //  namespace ui_conformance_testing
