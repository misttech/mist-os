// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/input/report/cpp/fidl.h>
#include <fuchsia/ui/display/singleton/cpp/fidl.h>
#include <fuchsia/ui/focus/cpp/fidl.h>
#include <fuchsia/ui/input/cpp/fidl.h>
#include <fuchsia/ui/input3/cpp/fidl.h>
#include <fuchsia/ui/policy/cpp/fidl.h>
#include <fuchsia/ui/test/conformance/cpp/fidl.h>
#include <fuchsia/ui/test/input/cpp/fidl.h>
#include <fuchsia/ui/test/scene/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <zircon/errors.h>

#include <sstream>
#include <string>
#include <utility>
#include <vector>

#include <gtest/gtest.h>
#include <src/lib/fostr/fidl/fuchsia/ui/input/formatting.h>

#include "src/ui/tests/conformance_input_tests/conformance-test-base.h"

namespace ui_conformance_testing {

const std::string PUPPET_UNDER_TEST_FACTORY_SERVICE = "puppet-under-test-factory-service";

namespace futi = fuchsia::ui::test::input;
namespace fir = fuchsia::input::report;
namespace fui = fuchsia::ui::input;
namespace fup = fuchsia::ui::policy;
namespace futc = fuchsia::ui::test::conformance;

class ButtonsListener final : public fup::MediaButtonsListener {
 public:
  ButtonsListener() : binding_(this) {}

  // |MediaButtonsListener|
  void OnEvent(fui::MediaButtonsEvent event, OnEventCallback callback) override {
    events_received_.push_back(std::move(event));
    callback();
  }
  void OnMediaButtonsEvent(fuchsia::ui::input::MediaButtonsEvent event) override {
    ZX_PANIC("Not Implemented");
  }
  // Returns a client end bound to this object.
  fidl::InterfaceHandle<fup::MediaButtonsListener> NewBinding() { return binding_.NewBinding(); }

  const std::vector<fui::MediaButtonsEvent>& events_received() const { return events_received_; }

  void clear_events() { events_received_.clear(); }

 private:
  fidl::Binding<fup::MediaButtonsListener> binding_;
  std::vector<fui::MediaButtonsEvent> events_received_;
};

class MediaButtonConformanceTest : public ui_conformance_test_base::ConformanceTest {
 public:
  ~MediaButtonConformanceTest() override = default;

  void SetUp() override {
    ui_conformance_test_base::ConformanceTest::SetUp();

    fuchsia::ui::views::ViewCreationToken root_view_token;
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

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fuchsia::ui::input3::Keyboard>();

      futc::PuppetFactoryCreateResponse resp;

      futc::PuppetCreationArgs creation_args;
      creation_args.set_server_end(puppet_ptr_.NewRequest());
      creation_args.set_view_token(std::move(root_view_token));
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(DevicePixelRatio());

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), futc::Result::SUCCESS);
    }

    // Register fake media button.
    {
      FX_LOGS(INFO) << "Connecting to input registry";
      auto input_registry = ConnectSyncIntoRealm<futi::Registry>();

      FX_LOGS(INFO) << "Registering fake media button device";
      futi::RegistryRegisterMediaButtonsDeviceRequest request;
      request.set_device(media_buttons_.NewRequest());
      ASSERT_EQ(input_registry->RegisterMediaButtonsDevice(std::move(request)), ZX_OK);
    }
  }

  void SimulatePress(fir::ConsumerControlButton button) {
    futi::MediaButtonsDeviceSimulateButtonPressRequest request;
    request.set_button(button);
    ASSERT_EQ(media_buttons_->SimulateButtonPress(std::move(request)), ZX_OK);
  }

  void SimulatePress(std::vector<fir::ConsumerControlButton> buttons) {
    futi::MediaButtonsDeviceSendButtonsStateRequest request;
    request.set_buttons(std::move(buttons));
    ASSERT_EQ(media_buttons_->SendButtonsState(std::move(request)), ZX_OK);
  }

 private:
  futi::MediaButtonsDeviceSyncPtr media_buttons_;
  futc::PuppetSyncPtr puppet_ptr_;
};

fui::MediaButtonsEvent MakeMediaButtonsEvent(int8_t volume, bool mic_mute, bool pause,
                                             bool camera_disable, bool power) {
  fui::MediaButtonsEvent e;
  e.set_volume(volume);
  e.set_mic_mute(mic_mute);
  e.set_pause(pause);
  e.set_camera_disable(camera_disable);
  e.set_power(power);
  return e;
}

fui::MediaButtonsEvent MakeEmptyEvent() {
  return MakeMediaButtonsEvent(0, false, false, false, false);
}

fui::MediaButtonsEvent MakeVolumeUpEvent() {
  return MakeMediaButtonsEvent(1, false, false, false, false);
}

fui::MediaButtonsEvent MakeVolumeDownEvent() {
  return MakeMediaButtonsEvent(-1, false, false, false, false);
}

fui::MediaButtonsEvent MakePauseEvent() {
  return MakeMediaButtonsEvent(0, false, true, false, false);
}

fui::MediaButtonsEvent MakeMicMuteEvent() {
  return MakeMediaButtonsEvent(0, true, false, false, false);
}

fui::MediaButtonsEvent MakeCameraDisableEvent() {
  return MakeMediaButtonsEvent(0, false, false, true, false);
}

fui::MediaButtonsEvent MakePowerEvent() {
  return MakeMediaButtonsEvent(0, false, false, false, true);
}

std::string ToString(const fui::MediaButtonsEvent& e) {
  std::stringstream ss;
  ss << e;
  return ss.str();
}

TEST_F(MediaButtonConformanceTest, SimplePress) {
  auto device_listener_registry = ConnectSyncIntoRealm<fup::DeviceListenerRegistry>();

  ButtonsListener listener;
  ASSERT_EQ(device_listener_registry->RegisterListener(listener.NewBinding()), ZX_OK);

  {
    SimulatePress(fir::ConsumerControlButton::VOLUME_UP);
    FX_LOGS(INFO) << "wait for button VOLUME_UP";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeVolumeUpEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  {
    SimulatePress(fir::ConsumerControlButton::VOLUME_DOWN);
    FX_LOGS(INFO) << "wait for button VOLUME_DOWN";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeVolumeDownEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  {
    SimulatePress(fir::ConsumerControlButton::PAUSE);
    FX_LOGS(INFO) << "wait for button PAUSE";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakePauseEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  {
    SimulatePress(fir::ConsumerControlButton::MIC_MUTE);
    FX_LOGS(INFO) << "wait for button MIC_MUTE";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeMicMuteEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  {
    SimulatePress(fir::ConsumerControlButton::CAMERA_DISABLE);
    FX_LOGS(INFO) << "wait for button CAMERA_DISABLE";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeCameraDisableEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  {
    SimulatePress(fir::ConsumerControlButton::FUNCTION);
    FX_LOGS(INFO) << "wait for button FUNCTION";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    // TODO(https://fxbug.dev/329271369): Soft-migrate function-button
    // functionality.
    // EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakePowerEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  // We did not handle following button yet.
  {
    SimulatePress(fir::ConsumerControlButton::POWER);
    FX_LOGS(INFO) << "wait for button POWER";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    // TODO(https://fxbug.dev/329271369): Soft-migrate power-button
    // functionality.
    // EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeEmptyEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  {
    SimulatePress(fir::ConsumerControlButton::REBOOT);
    FX_LOGS(INFO) << "wait for button REBOOT";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeEmptyEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }

  {
    SimulatePress(fir::ConsumerControlButton::FACTORY_RESET);
    FX_LOGS(INFO) << "wait for button FACTORY_RESET";
    RunLoopUntil([&listener]() { return listener.events_received().size() > 1; });
    EXPECT_EQ(listener.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeEmptyEvent()));
    EXPECT_EQ(ToString(listener.events_received()[1]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }
}

TEST_F(MediaButtonConformanceTest, MultiPress) {
  auto device_listener_registry = ConnectSyncIntoRealm<fup::DeviceListenerRegistry>();

  ButtonsListener listener;
  ASSERT_EQ(device_listener_registry->RegisterListener(listener.NewBinding()), ZX_OK);

  // press multi buttons.
  {
    auto buttons = std::vector<fir::ConsumerControlButton>{
        fuchsia::input::report::ConsumerControlButton::CAMERA_DISABLE,
        fuchsia::input::report::ConsumerControlButton::MIC_MUTE,
        fuchsia::input::report::ConsumerControlButton::PAUSE,
        fuchsia::input::report::ConsumerControlButton::VOLUME_UP,
    };
    SimulatePress(std::move(buttons));
    RunLoopUntil([&listener]() { return listener.events_received().size() > 0; });
    EXPECT_EQ(listener.events_received().size(), 1u);
    EXPECT_EQ(ToString(listener.events_received()[0]),
              ToString(MakeMediaButtonsEvent(1, true, true, true, false)));
    listener.clear_events();
  }

  // release multi buttons.
  {
    SimulatePress(std::vector<fir::ConsumerControlButton>());
    RunLoopUntil([&listener]() { return listener.events_received().size() > 0; });
    EXPECT_EQ(listener.events_received().size(), 1u);
    EXPECT_EQ(ToString(listener.events_received()[0]), ToString(MakeEmptyEvent()));
    listener.clear_events();
  }
}

TEST_F(MediaButtonConformanceTest, MultiListener) {
  auto device_listener_registry = ConnectSyncIntoRealm<fup::DeviceListenerRegistry>();

  ButtonsListener listener1;
  auto listener2 = std::make_unique<ButtonsListener>();
  ASSERT_EQ(device_listener_registry->RegisterListener(listener1.NewBinding()), ZX_OK);
  ASSERT_EQ(device_listener_registry->RegisterListener(listener2->NewBinding()), ZX_OK);

  // Both listener received events.
  {
    SimulatePress(fir::ConsumerControlButton::VOLUME_UP);
    FX_LOGS(INFO) << "wait for button VOLUME_UP";
    RunLoopUntil([&listener1]() { return listener1.events_received().size() > 1; });
    RunLoopUntil([&listener2]() { return listener2->events_received().size() > 1; });
    EXPECT_EQ(listener1.events_received().size(), 2u);
    EXPECT_EQ(listener2->events_received().size(), 2u);
    EXPECT_EQ(ToString(listener1.events_received()[0]), ToString(MakeVolumeUpEvent()));
    EXPECT_EQ(ToString(listener1.events_received()[1]), ToString(MakeEmptyEvent()));
    EXPECT_EQ(ToString(listener2->events_received()[0]), ToString(MakeVolumeUpEvent()));
    EXPECT_EQ(ToString(listener2->events_received()[1]), ToString(MakeEmptyEvent()));
    listener1.clear_events();
    listener2->clear_events();
  }

  ButtonsListener listener3;
  ASSERT_EQ(device_listener_registry->RegisterListener(listener3.NewBinding()), ZX_OK);

  // new listener receives the last event.
  {
    FX_LOGS(INFO) << "listener wait for the last button";
    RunLoopUntil([&listener3]() { return listener3.events_received().size() > 0; });
    EXPECT_EQ(ToString(listener3.events_received()[0]), ToString(MakeEmptyEvent()));
    listener3.clear_events();
  }

  // then new listener receive only new events.
  {
    SimulatePress(fir::ConsumerControlButton::VOLUME_DOWN);
    FX_LOGS(INFO) << "wait for button VOLUME_DOWN";
    RunLoopUntil([&listener1]() { return listener1.events_received().size() > 1; });
    RunLoopUntil([&listener2]() { return listener2->events_received().size() > 1; });
    RunLoopUntil([&listener3]() { return listener3.events_received().size() > 1; });
    EXPECT_EQ(listener1.events_received().size(), 2u);
    EXPECT_EQ(listener2->events_received().size(), 2u);
    EXPECT_EQ(listener3.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener1.events_received()[0]), ToString(MakeVolumeDownEvent()));
    EXPECT_EQ(ToString(listener1.events_received()[1]), ToString(MakeEmptyEvent()));
    EXPECT_EQ(ToString(listener2->events_received()[0]), ToString(MakeVolumeDownEvent()));
    EXPECT_EQ(ToString(listener2->events_received()[1]), ToString(MakeEmptyEvent()));
    EXPECT_EQ(ToString(listener3.events_received()[0]), ToString(MakeVolumeDownEvent()));
    EXPECT_EQ(ToString(listener3.events_received()[1]), ToString(MakeEmptyEvent()));
    listener1.clear_events();
    listener2->clear_events();
    listener3.clear_events();
  }

  // drop listener2, and verify other listeners still working.
  listener2 = {};
  {
    SimulatePress(fir::ConsumerControlButton::PAUSE);
    FX_LOGS(INFO) << "wait for button PAUSE";
    RunLoopUntil([&listener1]() { return listener1.events_received().size() > 1; });
    RunLoopUntil([&listener3]() { return listener3.events_received().size() > 1; });
    EXPECT_EQ(listener1.events_received().size(), 2u);
    EXPECT_EQ(listener3.events_received().size(), 2u);
    EXPECT_EQ(ToString(listener1.events_received()[0]), ToString(MakePauseEvent()));
    EXPECT_EQ(ToString(listener1.events_received()[1]), ToString(MakeEmptyEvent()));
    EXPECT_EQ(ToString(listener3.events_received()[0]), ToString(MakePauseEvent()));
    EXPECT_EQ(ToString(listener3.events_received()[1]), ToString(MakeEmptyEvent()));
    listener1.clear_events();
    listener3.clear_events();
  }
}

}  //  namespace ui_conformance_testing
