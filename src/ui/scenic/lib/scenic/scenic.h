// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCENIC_SCENIC_H_
#define SRC_UI_SCENIC_LIB_SCENIC_SCENIC_H_

#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <fuchsia/ui/scenic/internal/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fit/function.h>

#include <array>
#include <memory>
#include <unordered_map>
#include <vector>

#include "src/lib/fxl/macros.h"
#include "src/ui/scenic/lib/scenic/system.h"
#include "src/ui/scenic/lib/scheduling/id.h"

namespace scenic_impl {

// A Scenic instance has two main areas of responsibility:
//   - manage Session lifecycles
//   - provide a host environment for Services
class Scenic final : public fuchsia::ui::scenic::Scenic {
 public:
  // TODO(fxbug.dev/23686): Remove when we get rid of Scenic.GetDisplayInfo().
  class GetDisplayInfoDelegateDeprecated {
   public:
    virtual ~GetDisplayInfoDelegateDeprecated() = default;
    virtual void GetDisplayInfo(fuchsia::ui::scenic::Scenic::GetDisplayInfoCallback callback) = 0;
    virtual void GetDisplayOwnershipEvent(
        fuchsia::ui::scenic::Scenic::GetDisplayOwnershipEventCallback callback) = 0;
  };

  Scenic(sys::ComponentContext* app_context);
  ~Scenic();

  // |fuchsia::ui::scenic::Scenic|
  void GetDisplayInfo(fuchsia::ui::scenic::Scenic::GetDisplayInfoCallback callback) override;
  // |fuchsia::ui::scenic::Scenic|
  void TakeScreenshot(fuchsia::ui::scenic::Scenic::TakeScreenshotCallback callback) override;
  // |fuchsia::ui::scenic::Scenic|
  void GetDisplayOwnershipEvent(
      fuchsia::ui::scenic::Scenic::GetDisplayOwnershipEventCallback callback) override;
  // |fuchsia::ui::scenic::Scenic|
  void UsesFlatland(fuchsia::ui::scenic::Scenic::UsesFlatlandCallback callback) override;

  // |fuchsia::ui::scenic::Scenic|
  void CreateSession(fidl::InterfaceRequest<fuchsia::ui::scenic::Session> session,
                     fidl::InterfaceHandle<fuchsia::ui::scenic::SessionListener> listener) override;

  // |fuchsia::ui::scenic::Scenic|
  void CreateSession2(fidl::InterfaceRequest<fuchsia::ui::scenic::Session> session,
                      fidl::InterfaceHandle<fuchsia::ui::scenic::SessionListener> listener,
                      fidl::InterfaceRequest<fuchsia::ui::views::Focuser> view_focuser) override;

  // |fuchsia::ui::scenic::Scenic|
  void CreateSessionT(fuchsia::ui::scenic::SessionEndpoints endpoints,
                      CreateSessionTCallback callback) override;

  sys::ComponentContext* app_context() const { return app_context_; }

 private:
  sys::ComponentContext* const app_context_;
  fidl::BindingSet<fuchsia::ui::scenic::Scenic> scenic_bindings_;

  FXL_DISALLOW_COPY_AND_ASSIGN(Scenic);
};

}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_SCENIC_SCENIC_H_
