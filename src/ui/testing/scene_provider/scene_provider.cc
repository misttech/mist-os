// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/testing/scene_provider/scene_provider.h"

#include <fuchsia/session/scene/cpp/fidl.h>
#include <fuchsia/ui/app/cpp/fidl.h>
#include <lib/ui/scenic/cpp/view_ref_pair.h>
#include <lib/ui/scenic/cpp/view_token_pair.h>
#include <zircon/status.h>

#include "sdk/lib/syslog/cpp/macros.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/testing/scene_provider/scene_provider_config_lib.h"

namespace ui_testing {

void FakeViewController::Dismiss() { dismiss_(); }

SceneProvider::SceneProvider(sys::ComponentContext* context) : context_(context) {
  auto scene_provider_config = scene_provider_config_lib::Config::TakeFromStartupHandle();
  use_flatland_ = scene_provider_config.use_flatland();
  context_->svc()->Connect(scene_manager_.NewRequest());
}

void SceneProvider::AttachClientView(
    fuchsia::ui::test::scene::ControllerAttachClientViewRequest request,
    AttachClientViewCallback callback) {
  FX_LOGS(INFO) << "Attach client view";

  fuchsia::ui::views::ViewRef client_view_ref;

  fuchsia::session::scene::Manager_SetRootView_Result set_root_view_result;
  zx_status_t status = scene_manager_->SetRootView(std::move(*request.mutable_view_provider()),
                                                   &set_root_view_result);
  FX_CHECK(status == ZX_OK) << "Failed to call SetRootView: " << zx_status_get_string(status);
  FX_CHECK(set_root_view_result.is_response())
      << "Failed to set root view: " << set_root_view_result.err();
  client_view_ref = std::move(set_root_view_result.response().view_ref);

  callback(fsl::GetKoid(client_view_ref.reference.get()));
}

void SceneProvider::PresentClientView(
    fuchsia::ui::test::scene::ControllerPresentClientViewRequest request) {
  FX_CHECK(use_flatland_)
      << "Client attempted to present a view using a flatland token when GFX is enabled";
  FX_CHECK(request.has_viewport_creation_token()) << "Missing viewport creation token";

  fuchsia::session::scene::Manager_PresentRootView_Result present_root_view_result;
  scene_manager_->PresentRootView(std::move(*request.mutable_viewport_creation_token()),
                                  &present_root_view_result);

  FX_LOGS(INFO) << "Requested to present client view";
}

void SceneProvider::RegisterViewTreeWatcher(
    fidl::InterfaceRequest<fuchsia::ui::observation::geometry::ViewTreeWatcher> view_tree_watcher,
    RegisterViewTreeWatcherCallback callback) {
  // Register the client's view tree watcher.
  fuchsia::ui::observation::test::RegistrySyncPtr observer_registry;
  context_->svc()->Connect<fuchsia::ui::observation::test::Registry>(
      observer_registry.NewRequest());
  observer_registry->RegisterGlobalViewTreeWatcher(std::move(view_tree_watcher));

  callback();
}

// TODO(fxbug.dev/112819): Refactor to accommodate Flatland + Geometry
// Observation.
void SceneProvider::PresentView(
    fuchsia::element::ViewSpec view_spec,
    fidl::InterfaceHandle<fuchsia::element::AnnotationController> annotation_controller,
    fidl::InterfaceRequest<fuchsia::element::ViewController> view_controller,
    PresentViewCallback callback) {
  if (annotation_controller) {
    annotation_controller_.Bind(std::move(annotation_controller));
  }

  if (view_controller) {
    fake_view_controller_.emplace(std::move(view_controller), [this] { this->DismissView(); });
  }

  // TODO(fxbug.dev/106094): Register client's scoped view tree watcher, if
  // requested.

  // On GFX, |view_spec| will have the `view_ref` and `view_holder_token` fields
  // set. On flatland, it will have the `viewport_creation_token` field set.
  // Any other combination thereof is invalid.
  if (view_spec.has_view_ref() && view_spec.has_view_holder_token()) {
    FX_CHECK(!use_flatland_)
        << "Client attempted to present a view using GFX tokens when flatland is enabled";
    fuchsia::session::scene::Manager_PresentRootViewLegacy_Result set_root_view_result;
    zx_status_t status = scene_manager_->PresentRootViewLegacy(
        std::move(*view_spec.mutable_view_holder_token()), std::move(*view_spec.mutable_view_ref()),
        &set_root_view_result);
    FX_CHECK(status == ZX_OK) << "Failed to call PresentRootViewLegacy: "
                              << zx_status_get_string(status);
    FX_CHECK(set_root_view_result.is_response())
        << "Failed to present root view: " << set_root_view_result.err();
  } else if (view_spec.has_viewport_creation_token()) {
    FX_CHECK(use_flatland_)
        << "Client attempted to present a view using a flatland token when GFX is enabled";

    fuchsia::session::scene::Manager_PresentRootView_Result set_root_view_result;
    scene_manager_->PresentRootView(std::move(*view_spec.mutable_viewport_creation_token()),
                                    &set_root_view_result);
    if (set_root_view_result.is_err()) {
      FX_LOGS(ERROR) << "fuchsia.session.scene.Manager/PresentRootView error:"
                     << set_root_view_result.err();
      fuchsia::element::GraphicalPresenter_PresentView_Result result;
      result.set_err(fuchsia::element::PresentViewError::INVALID_ARGS);
      callback(std::move(result));
      return;
    }
  } else {
    FX_LOGS(FATAL) << "Invalid view spec";
  }

  fuchsia::element::GraphicalPresenter_PresentView_Result result;
  result.set_response({});
  callback(std::move(result));
}

fidl::InterfaceRequestHandler<fuchsia::ui::test::scene::Controller>
SceneProvider::GetSceneControllerHandler() {
  return scene_controller_bindings_.GetHandler(this);
}

fidl::InterfaceRequestHandler<fuchsia::element::GraphicalPresenter>
SceneProvider::GetGraphicalPresenterHandler() {
  return graphical_presenter_bindings_.GetHandler(this);
}

void SceneProvider::DismissView() {
  FX_CHECK(!use_flatland_)
      << "Dismissing views on flatland is not yet supported (fxbug.dev/114431)";

  // Give the scene provider a new ViewHolderToken to drop the existing view.
  auto client_view_tokens = scenic::ViewTokenPair::New();
  auto [client_control_ref, client_view_ref] = scenic::ViewRefPair::New();

  fuchsia::session::scene::Manager_PresentRootViewLegacy_Result set_root_view_result;

  zx_status_t status =
      scene_manager_->PresentRootViewLegacy(std::move(client_view_tokens.view_holder_token),
                                            fidl::Clone(client_view_ref), &set_root_view_result);
  FX_CHECK(status == ZX_OK) << "Failed to call PresentRootViewLegacy: "
                            << zx_status_get_string(status);
  FX_CHECK(set_root_view_result.is_response())
      << "Failed to present empty root view: " << set_root_view_result.err();
}

}  // namespace ui_testing
