// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/scenic/scenic.h"

#include <lib/async/default.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

namespace scenic_impl {

using fuchsia::ui::scenic::SessionEndpoints;

Scenic::Scenic(sys::ComponentContext* app_context) : app_context_(app_context) {
  FX_DCHECK(app_context_);
  app_context->outgoing()->AddPublicService(scenic_bindings_.GetHandler(this));
}

Scenic::~Scenic() = default;

void Scenic::CreateSession(fidl::InterfaceRequest<fuchsia::ui::scenic::Session> session_request,
                           fidl::InterfaceHandle<fuchsia::ui::scenic::SessionListener> listener) {
  FX_LOGS(WARNING) << "CreateSession is ineffective in Flatland.";
}

void Scenic::CreateSession2(fidl::InterfaceRequest<fuchsia::ui::scenic::Session> session_request,
                            fidl::InterfaceHandle<fuchsia::ui::scenic::SessionListener> listener,
                            fidl::InterfaceRequest<fuchsia::ui::views::Focuser> view_focuser) {
  FX_LOGS(WARNING) << "CreateSession2 is ineffective in Flatland.";
}

void Scenic::CreateSessionT(SessionEndpoints endpoints, CreateSessionTCallback callback) {
  FX_LOGS(WARNING) << "CreateSessionT is ineffective in Flatland.";
  callback();
}

void Scenic::GetDisplayInfo(fuchsia::ui::scenic::Scenic::GetDisplayInfoCallback callback) {
  FX_LOGS(WARNING) << "GetDisplayInfo is ineffective in Flatland.";
  fuchsia::ui::gfx::DisplayInfo data;
  callback(std::move(data));
}

void Scenic::TakeScreenshot(fuchsia::ui::scenic::Scenic::TakeScreenshotCallback callback) {
  FX_LOGS(WARNING) << "TakeScreenshot is ineffective in Flatland.";
  fuchsia::ui::scenic::ScreenshotData data;
  callback(std::move(data), true);
}

void Scenic::GetDisplayOwnershipEvent(
    fuchsia::ui::scenic::Scenic::GetDisplayOwnershipEventCallback callback) {
  FX_LOGS(WARNING) << "GetDisplayInfo is ineffective in Flatland.";
  zx::event event;
  callback(std::move(event));
}

void Scenic::UsesFlatland(fuchsia::ui::scenic::Scenic::UsesFlatlandCallback callback) {
  callback(true);
}

}  // namespace scenic_impl
