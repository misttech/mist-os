// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/display/singleton/cpp/fidl.h>
#include <fuchsia/ui/focus/cpp/fidl.h>
#include <fuchsia/ui/input3/cpp/fidl.h>
#include <fuchsia/ui/test/conformance/cpp/fidl.h>
#include <fuchsia/ui/test/input/cpp/fidl.h>
#include <fuchsia/ui/test/scene/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <zircon/errors.h>

#include <optional>
#include <string>
#include <utility>
#include <vector>

#include <gtest/gtest.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/tests/conformance/conformance-test-base.h"

namespace ui_conformance_testing {

const std::string PUPPET_UNDER_TEST_FACTORY_SERVICE = "puppet-under-test-factory-service";
const std::string AUXILIARY_PUPPET_FACTORY_SERVICE = "auxiliary-puppet-factory-service";

namespace futi = fuchsia::ui::test::input;
namespace futc = fuchsia::ui::test::conformance;
namespace fui = fuchsia::ui::input3;
namespace fuv = fuchsia::ui::views;
namespace fuf = fuchsia::ui::focus;

class FocusListener : public fuf::FocusChainListener {
 public:
  FocusListener() : binding_(this) {}

  // |fuchsia::ui::focus::FocusChainListener|
  void OnFocusChange(fuf::FocusChain focus_chain, OnFocusChangeCallback callback) override {
    last_focus_chain_ = std::move(focus_chain);
    callback();
  }

  // Returns a client end bound to this object.
  fidl::InterfaceHandle<fuf::FocusChainListener> NewBinding() { return binding_.NewBinding(); }

  bool IsViewFocused(const fuv::ViewRef& view_ref) const {
    if (!last_focus_chain_) {
      return false;
    }

    if (!last_focus_chain_->has_focus_chain()) {
      return false;
    }

    if (last_focus_chain_->focus_chain().empty()) {
      return false;
    }

    // the new focus view store at the last slot.
    return fsl::GetKoid(last_focus_chain_->focus_chain().back().reference.get()) ==
           fsl::GetKoid(view_ref.reference.get());
  }

 private:
  fidl::Binding<fuf::FocusChainListener> binding_;
  // Holds the most recent focus chain update received.
  std::optional<fuchsia::ui::focus::FocusChain> last_focus_chain_;
};

class KeyListener : public futi::KeyboardInputListener {
 public:
  KeyListener() : binding_(this) {}

  // |fuchsia::ui::test::input::KeyboardInputListener|
  void ReportTextInput(futi::KeyboardInputListenerReportTextInputRequest request) override {
    events_received_.push_back(std::move(request));
  }

  // |fuchsia::ui::test::input::KeyboardInputListener|
  void ReportReady(futi::KeyboardInputListener::ReportReadyCallback callback) override {
    // Puppet factory create view already wait for keyboard ready.
  }

  // Returns a client end bound to this object.
  fidl::InterfaceHandle<futi::KeyboardInputListener> NewBinding() { return binding_.NewBinding(); }

  const std::vector<futi::KeyboardInputListenerReportTextInputRequest>& events_received() {
    return events_received_;
  }

  void clear_events() { events_received_.clear(); }

 private:
  fidl::Binding<futi::KeyboardInputListener> binding_;
  std::vector<futi::KeyboardInputListenerReportTextInputRequest> events_received_;
};

// Holds resources associated with a single puppet instance.
struct KeyPuppet {
  futc::PuppetSyncPtr puppet_ptr;
  KeyListener key_listener;
};

class KeyConformanceTest : public ui_conformance_test_base::ConformanceTest {
 public:
  ~KeyConformanceTest() override = default;

  void SetUp() override {
    ui_conformance_test_base::ConformanceTest::SetUp();

    // Register fake keyboard.
    {
      FX_LOGS(INFO) << "Connecting to input registry";
      auto input_registry = ConnectSyncIntoRealm<futi::Registry>();

      FX_LOGS(INFO) << "Registering fake keyboard";
      futi::RegistryRegisterKeyboardRequest request;
      request.set_device(fake_keyboard_.NewRequest());
      ASSERT_EQ(input_registry->RegisterKeyboard(std::move(request)), ZX_OK);
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

    // Get root view token.
    {
      FX_LOGS(INFO) << "Creating root view token";

      auto controller = ConnectSyncIntoRealm<fuchsia::ui::test::scene::Controller>();

      fuchsia::ui::test::scene::ControllerPresentClientViewRequest req;
      auto [view_token, viewport_token] = scenic::ViewCreationTokenPair::New();
      req.set_viewport_creation_token(std::move(viewport_token));
      ASSERT_EQ(controller->PresentClientView(std::move(req)), ZX_OK);
      root_view_token_ = std::move(view_token);
    }

    // Setup focus listener.
    {
      auto focus_registry = ConnectSyncIntoRealm<fuf::FocusChainListenerRegistry>();
      ASSERT_EQ(focus_registry->Register(focus_listener_.NewBinding()), ZX_OK);
    }
  }

  void SimulateKeyEvent(std::string str) {
    FX_LOGS(INFO) << "Requesting key event";
    futi::KeyboardSimulateUsAsciiTextEntryRequest request;
    request.set_text(std::move(str));

    fake_keyboard_->SimulateUsAsciiTextEntry(std::move(request));
    FX_LOGS(INFO) << "Key event injected";
  }

 protected:
  int32_t display_width_as_int() const { return static_cast<int32_t>(display_width_); }
  int32_t display_height_as_int() const { return static_cast<int32_t>(display_height_); }

  futi::KeyboardSyncPtr fake_keyboard_;
  fuv::ViewCreationToken root_view_token_;
  FocusListener focus_listener_;
  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
};

class SingleViewKeyConformanceTest : public KeyConformanceTest {
 public:
  ~SingleViewKeyConformanceTest() override = default;

  void SetUp() override {
    KeyConformanceTest::SetUp();

    {
      FX_LOGS(INFO) << "Create puppet under test";
      futc::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 PUPPET_UNDER_TEST_FACTORY_SERVICE),
                ZX_OK);

      futc::PuppetFactoryCreateResponse resp;

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fui::Keyboard>();

      futc::PuppetCreationArgs creation_args;
      creation_args.set_server_end(puppet_.puppet_ptr.NewRequest());
      creation_args.set_view_token(std::move(root_view_token_));
      creation_args.set_keyboard_listener(puppet_.key_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(1.0);

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), futc::Result::SUCCESS);

      FX_LOGS(INFO) << "wait for focus";
      RunLoopUntil(
          [this, &resp]() { return this->focus_listener_.IsViewFocused(resp.view_ref()); });
    }
  }

 protected:
  KeyPuppet puppet_;
};

TEST_F(SingleViewKeyConformanceTest, SimpleKeyPress) {
  SimulateKeyEvent("Hello\nWorld!");
  FX_LOGS(INFO) << "Wait for puppet to report key events";
  RunLoopUntil([this]() { return this->puppet_.key_listener.events_received().size() >= 15; });

  ASSERT_EQ(puppet_.key_listener.events_received().size(), 15u);

  const auto& events = puppet_.key_listener.events_received();
  EXPECT_EQ(events[0].non_printable(), fui::NonPrintableKey::SHIFT);
  // view reports value from key meaning so it is Shifted h (H).
  EXPECT_EQ(events[1].text(), std::string("H"));
  EXPECT_EQ(events[2].text(), std::string("e"));
  EXPECT_EQ(events[3].text(), std::string("l"));
  EXPECT_EQ(events[4].text(), std::string("l"));
  EXPECT_EQ(events[5].text(), std::string("o"));
  EXPECT_EQ(events[6].non_printable(), fui::NonPrintableKey::ENTER);
  EXPECT_EQ(events[7].non_printable(), fui::NonPrintableKey::SHIFT);
  EXPECT_EQ(events[8].text(), std::string("W"));
  EXPECT_EQ(events[9].text(), std::string("o"));
  EXPECT_EQ(events[10].text(), std::string("r"));
  EXPECT_EQ(events[11].text(), std::string("l"));
  EXPECT_EQ(events[12].text(), std::string("d"));
  EXPECT_EQ(events[13].non_printable(), fui::NonPrintableKey::SHIFT);
  EXPECT_EQ(events[14].text(), std::string("!"));
}

class EmbeddedViewKeyConformanceTest : public KeyConformanceTest {
 public:
  ~EmbeddedViewKeyConformanceTest() override = default;

  void SetUp() override {
    KeyConformanceTest::SetUp();

    {
      FX_LOGS(INFO) << "Create parent puppet";

      futc::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 AUXILIARY_PUPPET_FACTORY_SERVICE),
                ZX_OK);

      futc::PuppetFactoryCreateResponse resp;

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fui::Keyboard>();

      futc::PuppetCreationArgs creation_args;
      creation_args.set_server_end(parent_puppet_.puppet_ptr.NewRequest());
      creation_args.set_view_token(std::move(root_view_token_));
      creation_args.set_keyboard_listener(parent_puppet_.key_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(1.0);
      creation_args.set_focuser(parent_focuser_.NewRequest());

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), futc::Result::SUCCESS);
      ASSERT_TRUE(resp.has_view_ref());
      resp.view_ref().Clone(&parent_view_ref_);
    }

    // Create child viewport.
    futc::PuppetEmbedRemoteViewResponse embed_remote_view_response;
    {
      FX_LOGS(INFO) << "Creating child viewport";
      const uint64_t kChildViewportId = 1u;
      futc::PuppetEmbedRemoteViewRequest embed_remote_view_request;
      embed_remote_view_request.set_id(kChildViewportId);
      embed_remote_view_request.mutable_properties()->mutable_bounds()->set_size(
          {.width = display_width_ / 2, .height = display_height_ / 2});
      embed_remote_view_request.mutable_properties()->mutable_bounds()->set_origin(
          {.x = display_width_as_int() / 2, .y = display_height_as_int() / 2});
      parent_puppet_.puppet_ptr->EmbedRemoteView(std::move(embed_remote_view_request),
                                                 &embed_remote_view_response);
    }

    // Create child view.
    {
      FX_LOGS(INFO) << "Creating child puppet";
      futc::PuppetFactorySyncPtr puppet_factory;

      ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                                 PUPPET_UNDER_TEST_FACTORY_SERVICE),
                ZX_OK);

      futc::PuppetFactoryCreateResponse resp;

      auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
      auto keyboard = ConnectSyncIntoRealm<fui::Keyboard>();

      futc::PuppetCreationArgs creation_args;
      creation_args.set_server_end(child_puppet_.puppet_ptr.NewRequest());
      creation_args.set_view_token(
          std::move(*embed_remote_view_response.mutable_view_creation_token()));
      creation_args.set_keyboard_listener(child_puppet_.key_listener.NewBinding());
      creation_args.set_flatland_client(std::move(flatland));
      creation_args.set_keyboard_client(std::move(keyboard));
      creation_args.set_device_pixel_ratio(1.0);

      ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
      ASSERT_EQ(resp.result(), futc::Result::SUCCESS);
      ASSERT_TRUE(resp.has_view_ref());
      resp.view_ref().Clone(&child_view_ref_);
    }
  }

 protected:
  KeyPuppet parent_puppet_;
  fuv::FocuserSyncPtr parent_focuser_;
  fuv::ViewRef parent_view_ref_;

  KeyPuppet child_puppet_;
  fuv::ViewRef child_view_ref_;
};

TEST_F(EmbeddedViewKeyConformanceTest, KeyToFocusedView) {
  // Default is focus on parent view.
  {
    FX_LOGS(INFO) << "wait for focus to parent";
    RunLoopUntil([this]() { return this->focus_listener_.IsViewFocused(this->parent_view_ref_); });

    // Inject key events, and expect parent view receives events.
    SimulateKeyEvent("a");
    FX_LOGS(INFO) << "Wait for parent puppet to report key events";
    RunLoopUntil(
        [this]() { return this->parent_puppet_.key_listener.events_received().size() >= 1; });

    ASSERT_EQ(parent_puppet_.key_listener.events_received().size(), 1u);

    const auto& events = parent_puppet_.key_listener.events_received();
    EXPECT_EQ(events[0].text(), "a");
    EXPECT_TRUE(child_puppet_.key_listener.events_received().empty());
    parent_puppet_.key_listener.clear_events();
  }

  // Focus on child view.
  {
    fuv::ViewRef child_view_ref;
    child_view_ref_.Clone(&child_view_ref);

    fuv::Focuser_RequestFocus_Result res;
    ASSERT_EQ(parent_focuser_->RequestFocus(std::move(child_view_ref), &res), ZX_OK);

    FX_LOGS(INFO) << "wait for focus to child";
    RunLoopUntil([this]() { return this->focus_listener_.IsViewFocused(this->child_view_ref_); });

    // Inject key events, and expect child view receives events.
    SimulateKeyEvent("b");

    FX_LOGS(INFO) << "Wait for child puppet to report key events";
    RunLoopUntil(
        [this]() { return this->child_puppet_.key_listener.events_received().size() >= 1; });

    ASSERT_EQ(child_puppet_.key_listener.events_received().size(), 1u);

    const auto& events = child_puppet_.key_listener.events_received();
    EXPECT_EQ(events[0].text(), "b");
    EXPECT_TRUE(parent_puppet_.key_listener.events_received().empty());
    child_puppet_.key_listener.clear_events();
  }

  // Focus back to parent view.
  {
    fuv::ViewRef parent_view_ref;
    parent_view_ref_.Clone(&parent_view_ref);

    fuv::Focuser_RequestFocus_Result res;
    ASSERT_EQ(parent_focuser_->RequestFocus(std::move(parent_view_ref), &res), ZX_OK);

    FX_LOGS(INFO) << "wait for focus to parent";
    RunLoopUntil([this]() { return this->focus_listener_.IsViewFocused(this->parent_view_ref_); });

    // Inject key events, and expect parent view receives events.
    SimulateKeyEvent("c");
    FX_LOGS(INFO) << "Wait for parent puppet to report key events";
    RunLoopUntil(
        [this]() { return this->parent_puppet_.key_listener.events_received().size() >= 1; });

    ASSERT_EQ(parent_puppet_.key_listener.events_received().size(), 1u);

    const auto& events = parent_puppet_.key_listener.events_received();
    EXPECT_EQ(events[0].text(), "c");
    EXPECT_TRUE(child_puppet_.key_listener.events_received().empty());
    parent_puppet_.key_listener.clear_events();
  }
}

}  //  namespace ui_conformance_testing
