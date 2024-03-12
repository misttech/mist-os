// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/display/singleton/cpp/fidl.h>
#include <fuchsia/ui/focus/cpp/fidl.h>
#include <fuchsia/ui/input3/cpp/fidl.h>
#include <fuchsia/ui/test/conformance/cpp/fidl.h>
#include <fuchsia/ui/test/scene/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <zircon/errors.h>

#include <string>
#include <utility>
#include <vector>

#include <gtest/gtest.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/tests/conformance_input_tests/conformance-test-base.h"

namespace ui_conformance_testing {

const std::string PUPPET_UNDER_TEST_FACTORY_SERVICE = "puppet-under-test-factory-service";

namespace futc = fuchsia::ui::test::conformance;
namespace fui = fuchsia::ui::input3;
namespace fuv = fuchsia::ui::views;
namespace fuf = fuchsia::ui::focus;

class FocusListener : public fuf::FocusChainListener {
 public:
  FocusListener() : binding_(this) {}

  // |fuchsia::ui::focus::FocusChainListener|
  void OnFocusChange(fuf::FocusChain focus_chain, OnFocusChangeCallback callback) override {
    focus_chain_updates_.push_back(std::move(focus_chain));
    callback();
  }

  // Returns a client end bound to this object.
  fidl::InterfaceHandle<fuf::FocusChainListener> NewBinding() { return binding_.NewBinding(); }

  bool IsViewFocused(const fuv::ViewRef& view_ref) const {
    if (focus_chain_updates_.empty()) {
      return false;
    }

    const auto& last_focus_chain = focus_chain_updates_.back();

    if (!last_focus_chain.has_focus_chain()) {
      return false;
    }

    if (last_focus_chain.focus_chain().empty()) {
      return false;
    }

    // the new focus view store at the last slot.
    return fsl::GetKoid(last_focus_chain.focus_chain().back().reference.get()) ==
           fsl::GetKoid(view_ref.reference.get());
  }

  const std::vector<fuf::FocusChain>& focus_chain_updates() const { return focus_chain_updates_; }

 private:
  fidl::Binding<fuf::FocusChainListener> binding_;
  std::vector<fuf::FocusChain> focus_chain_updates_;
};

using FocusConformanceTest = ui_conformance_test_base::ConformanceTest;

// This test exercises the focus contract with the scene owner: the view offered to the
// scene owner will have focus transferred to it.
TEST_F(FocusConformanceTest, ReceivesFocusTransfer) {
  // Setup focus listener.
  FocusListener focus_listener;
  {
    auto focus_registry = ConnectSyncIntoRealm<fuf::FocusChainListenerRegistry>();
    ASSERT_EQ(focus_registry->Register(focus_listener.NewBinding()), ZX_OK);
  }

  RunLoopUntil([&focus_listener]() { return focus_listener.focus_chain_updates().size() > 0; });

  // No focus chain when no scene.
  EXPECT_FALSE(focus_listener.focus_chain_updates().back().has_focus_chain());

  // Present view.
  fuv::ViewCreationToken root_view_token;
  {
    FX_LOGS(INFO) << "Creating root view token";

    auto controller = ConnectSyncIntoRealm<fuchsia::ui::test::scene::Controller>();

    fuchsia::ui::test::scene::ControllerPresentClientViewRequest req;
    auto [view_token, viewport_token] = scenic::ViewCreationTokenPair::New();
    req.set_viewport_creation_token(std::move(viewport_token));
    ASSERT_EQ(controller->PresentClientView(std::move(req)), ZX_OK);
    root_view_token = std::move(view_token);
  }

  // Add puppet view.
  fuv::ViewRef view_ref;
  {
    FX_LOGS(INFO) << "Create puppet under test";
    futc::PuppetFactorySyncPtr puppet_factory;

    ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                               PUPPET_UNDER_TEST_FACTORY_SERVICE),
              ZX_OK);

    futc::PuppetFactoryCreateResponse resp;

    auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
    auto keyboard = ConnectSyncIntoRealm<fui::Keyboard>();
    futc::PuppetSyncPtr puppet_ptr;

    futc::PuppetCreationArgs creation_args;
    creation_args.set_server_end(puppet_ptr.NewRequest());
    creation_args.set_view_token(std::move(root_view_token));
    creation_args.set_flatland_client(std::move(flatland));
    creation_args.set_keyboard_client(std::move(keyboard));
    creation_args.set_device_pixel_ratio(1.0);

    ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
    ASSERT_EQ(resp.result(), futc::Result::SUCCESS);

    resp.view_ref().Clone(&view_ref);
  }

  FX_LOGS(INFO) << "wait for focus";
  RunLoopUntil([&focus_listener, &view_ref]() { return focus_listener.IsViewFocused(view_ref); });
}

// This test ensures that multiple clients can connect to the FocusChainListenerRegistry.
TEST_F(FocusConformanceTest, MultiListener) {
  // Setup focus listener a.
  FocusListener focus_listener_a;
  {
    auto focus_registry = ConnectSyncIntoRealm<fuf::FocusChainListenerRegistry>();
    ASSERT_EQ(focus_registry->Register(focus_listener_a.NewBinding()), ZX_OK);
  }

  RunLoopUntil([&focus_listener_a]() { return focus_listener_a.focus_chain_updates().size() > 0; });

  // No focus chain when no scene.
  EXPECT_FALSE(focus_listener_a.focus_chain_updates().back().has_focus_chain());

  // Present view.
  fuv::ViewCreationToken root_view_token;
  {
    FX_LOGS(INFO) << "Creating root view token";

    auto controller = ConnectSyncIntoRealm<fuchsia::ui::test::scene::Controller>();

    fuchsia::ui::test::scene::ControllerPresentClientViewRequest req;
    auto [view_token, viewport_token] = scenic::ViewCreationTokenPair::New();
    req.set_viewport_creation_token(std::move(viewport_token));
    ASSERT_EQ(controller->PresentClientView(std::move(req)), ZX_OK);
    root_view_token = std::move(view_token);
  }

  // Setup focus listener b.
  FocusListener focus_listener_b;
  {
    auto focus_registry = ConnectSyncIntoRealm<fuf::FocusChainListenerRegistry>();
    ASSERT_EQ(focus_registry->Register(focus_listener_b.NewBinding()), ZX_OK);
  }

  // Add puppet view.
  fuv::ViewRef view_ref;
  {
    FX_LOGS(INFO) << "Create puppet under test";
    futc::PuppetFactorySyncPtr puppet_factory;

    ASSERT_EQ(LocalServiceDirectory()->Connect(puppet_factory.NewRequest(),
                                               PUPPET_UNDER_TEST_FACTORY_SERVICE),
              ZX_OK);

    futc::PuppetFactoryCreateResponse resp;

    auto flatland = ConnectSyncIntoRealm<fuchsia::ui::composition::Flatland>();
    auto keyboard = ConnectSyncIntoRealm<fui::Keyboard>();
    futc::PuppetSyncPtr puppet_ptr;

    futc::PuppetCreationArgs creation_args;
    creation_args.set_server_end(puppet_ptr.NewRequest());
    creation_args.set_view_token(std::move(root_view_token));
    creation_args.set_flatland_client(std::move(flatland));
    creation_args.set_keyboard_client(std::move(keyboard));
    creation_args.set_device_pixel_ratio(1.0);

    ASSERT_EQ(puppet_factory->Create(std::move(creation_args), &resp), ZX_OK);
    ASSERT_EQ(resp.result(), futc::Result::SUCCESS);

    resp.view_ref().Clone(&view_ref);
  }

  FX_LOGS(INFO) << "focus listener a receives focus transfer";
  RunLoopUntil(
      [&focus_listener_a, &view_ref]() { return focus_listener_a.IsViewFocused(view_ref); });

  FX_LOGS(INFO) << "focus listener b receives focus transfer";
  RunLoopUntil(
      [&focus_listener_b, &view_ref]() { return focus_listener_b.IsViewFocused(view_ref); });
}

}  //  namespace ui_conformance_testing
