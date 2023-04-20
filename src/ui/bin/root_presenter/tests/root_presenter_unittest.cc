// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/focus/cpp/fidl.h>
#include <fuchsia/ui/input/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <lib/ui/scenic/cpp/commands.h>
#include <lib/ui/scenic/cpp/view_token_pair.h>
#include <zircon/status.h>

#include <gtest/gtest.h>
#include <src/lib/fxl/macros.h>
#include <src/lib/fxl/strings/join_strings.h>
#include <src/lib/testing/loop_fixture/test_loop_fixture.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/bin/root_presenter/app.h"
#include "src/ui/bin/root_presenter/presentation.h"
#include "src/ui/bin/root_presenter/tests/fakes/fake_keyboard_focus_controller.h"
#include "src/ui/bin/root_presenter/tests/fakes/fake_view.h"

namespace root_presenter {
namespace {

class RootPresenterTest : public gtest::RealLoopFixture,
                          public fuchsia::ui::focus::FocusChainListener {
 public:
  RootPresenterTest() : focus_listener_(this) {}

  void SetUp() final {
    real_component_context_ = sys::ComponentContext::CreateAndServeOutgoingDirectory();

    // Proxy real APIs through the fake component_context.
    // TODO(fxbug.dev/74262): The test should set up a test environment instead of
    // injecting a real scenic in the sandbox.
    ASSERT_EQ(
        ZX_OK,
        context_provider_.service_directory_provider()->AddService<fuchsia::ui::scenic::Scenic>(
            [this](fidl::InterfaceRequest<fuchsia::ui::scenic::Scenic> request) {
              real_component_context_->svc()->Connect(std::move(request));
            }));
    // Connect FocusChainListenerRegistry to the real Scenic injected in the test sandbox.
    ASSERT_EQ(ZX_OK,
              context_provider_.service_directory_provider()
                  ->AddService<fuchsia::ui::focus::FocusChainListenerRegistry>(
                      [this](fidl::InterfaceRequest<fuchsia::ui::focus::FocusChainListenerRegistry>
                                 request) {
                        real_component_context_->svc()->Connect(std::move(request));
                      }));

    keyboard_focus_ctl_ = std::make_unique<testing::FakeKeyboardFocusController>(context_provider_);

    // Start RootPresenter with fake context.
    root_presenter_ = std::make_unique<App>(context_provider_.context(), [this] { QuitLoop(); });
  }

  void TearDown() final { root_presenter_.reset(); }

  App* root_presenter() { return root_presenter_.get(); }
  Presentation* presentation() { return root_presenter_->presentation(); }

  void ConnectInjectorRegistry() {
    ASSERT_EQ(
        ZX_OK,
        context_provider_.service_directory_provider()
            ->AddService<fuchsia::ui::pointerinjector::Registry>(
                [this](fidl::InterfaceRequest<fuchsia::ui::pointerinjector::Registry> request) {
                  real_component_context_->svc()->Connect(std::move(request));
                }));
  }

  void SetUpFocusChainListener(
      fit::function<void(fuchsia::ui::focus::FocusChain focus_chain)> callback) {
    focus_callback_ = std::move(callback);

    fuchsia::ui::focus::FocusChainListenerRegistryPtr focus_chain_listener_registry;
    real_component_context_->svc()->Connect(focus_chain_listener_registry.NewRequest());
    focus_chain_listener_registry.set_error_handler([](zx_status_t status) {
      FX_LOGS(ERROR) << "FocusChainListenerRegistry connection failed with status: "
                     << zx_status_get_string(status);
      FAIL();
    });
    focus_chain_listener_registry->Register(focus_listener_.NewBinding());

    RunLoopUntil([this] { return focus_set_up_; });
  }

  // |fuchsia.ui.focus.FocusChainListener|
  void OnFocusChange(fuchsia::ui::focus::FocusChain focus_chain,
                     OnFocusChangeCallback callback) override {
    focus_set_up_ = true;
    focus_callback_(std::move(focus_chain));
    callback();
  }

  std::vector<inspect::UintArrayValue::HistogramBucket> GetHistogramBuckets(
      const std::vector<std::string>& path, const std::string& property) {
    inspect::Hierarchy root =
        inspect::ReadFromVmo(root_presenter()->inspector()->CopyVmo().value()).take_value();

    const inspect::Hierarchy* parent = root.GetByPath(path);
    FX_CHECK(parent) << "no node found at path " << fxl::JoinStrings(path, "/");
    const inspect::UintArrayValue* histogram =
        parent->node().get_property<inspect::UintArrayValue>(property);
    FX_CHECK(histogram) << "no histogram named " << property << " in node with path "
                        << fxl::JoinStrings(path, "/");
    return histogram->GetBuckets();
  }

  std::unique_ptr<testing::FakeKeyboardFocusController> keyboard_focus_ctl_;
  sys::testing::ComponentContextProvider context_provider_;

 private:
  std::unique_ptr<sys::ComponentContext> real_component_context_;
  std::unique_ptr<App> root_presenter_;

  fidl::Binding<fuchsia::ui::focus::FocusChainListener> focus_listener_;
  fit::function<void(fuchsia::ui::focus::FocusChain focus_chain)> focus_callback_;
  bool focus_set_up_ = false;

  fuchsia::ui::views::ViewToken view_token_;
};

TEST_F(RootPresenterTest, TestSceneSetup) {
  // Present a fake view.
  fuchsia::ui::scenic::ScenicPtr scenic =
      context_provider_.context()->svc()->Connect<fuchsia::ui::scenic::Scenic>();
  testing::FakeView fake_view(context_provider_.context(), std::move(scenic));
  presentation()->PresentView(fake_view.view_holder_token(), nullptr);

  // Run until the view is attached to the scene.
  RunLoopUntil([&fake_view]() { return fake_view.IsAttachedToScene(); });
}

TEST_F(RootPresenterTest, SinglePresentView_ShouldSucceed) {
  auto [view_token, view_holder_token] = scenic::ViewTokenPair::New();

  fidl::InterfacePtr<fuchsia::ui::policy::Presentation> presentation_ptr;
  bool alive = true;
  presentation_ptr.set_error_handler([&alive](auto) { alive = false; });
  presentation()->PresentView(std::move(view_holder_token), presentation_ptr.NewRequest());

  RunLoopUntilIdle();

  EXPECT_TRUE(alive);
}

TEST_F(RootPresenterTest, SecondPresentView_ShouldFail_AndOriginalShouldSurvive) {
  auto [view_token1, view_holder_token1] = scenic::ViewTokenPair::New();
  auto [view_token2, view_holder_token2] = scenic::ViewTokenPair::New();

  fidl::InterfacePtr<fuchsia::ui::policy::Presentation> presentation1;
  bool alive1 = true;
  presentation1.set_error_handler([&alive1](auto) { alive1 = false; });
  presentation()->PresentView(std::move(view_holder_token1), presentation1.NewRequest());

  fidl::InterfacePtr<fuchsia::ui::policy::Presentation> presentation2;
  bool alive2 = true;
  zx_status_t error = ZX_OK;
  presentation2.set_error_handler([&alive2, &error](zx_status_t err) {
    alive2 = false;
    error = err;
  });
  presentation()->PresentView(std::move(view_holder_token2), presentation2.NewRequest());

  RunLoopUntilIdle();

  EXPECT_TRUE(alive1);
  EXPECT_FALSE(alive2);
  EXPECT_EQ(error, ZX_ERR_ALREADY_BOUND)
      << "Should be: " << zx_status_get_string(ZX_ERR_ALREADY_BOUND)
      << " Was: " << zx_status_get_string(error);
}

TEST_F(RootPresenterTest, SinglePresentOrReplaceView_ShouldSucceeed) {
  auto [view_token, view_holder_token] = scenic::ViewTokenPair::New();

  fidl::InterfacePtr<fuchsia::ui::policy::Presentation> presentation_ptr;
  bool alive = true;
  presentation_ptr.set_error_handler([&alive](auto) { alive = false; });
  presentation()->PresentView(std::move(view_holder_token), presentation_ptr.NewRequest());

  RunLoopUntilIdle();

  EXPECT_TRUE(alive);
}

TEST_F(RootPresenterTest, SecondPresentOrReplaceView_ShouldSucceeed_AndOriginalShouldDie) {
  auto [view_token1, view_holder_token1] = scenic::ViewTokenPair::New();
  auto [view_token2, view_holder_token2] = scenic::ViewTokenPair::New();

  fidl::InterfacePtr<fuchsia::ui::policy::Presentation> presentation1;
  bool alive1 = true;
  zx_status_t error = ZX_OK;
  presentation1.set_error_handler([&alive1, &error](zx_status_t err) {
    alive1 = false;
    error = err;
  });
  presentation()->PresentOrReplaceView(std::move(view_holder_token1), presentation1.NewRequest());

  fidl::InterfacePtr<fuchsia::ui::policy::Presentation> presentation2;
  bool alive2 = true;
  presentation2.set_error_handler([&alive2](auto) { alive2 = false; });
  presentation()->PresentOrReplaceView(std::move(view_holder_token2), presentation2.NewRequest());

  RunLoopUntilIdle();

  EXPECT_FALSE(alive1);
  EXPECT_EQ(error, ZX_ERR_PEER_CLOSED) << "Should be: " << zx_status_get_string(ZX_ERR_PEER_CLOSED)
                                       << " Was: " << zx_status_get_string(error);
  EXPECT_TRUE(alive2);
}

}  // namespace
}  // namespace root_presenter
