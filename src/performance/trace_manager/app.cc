// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/app.h"

#include <lib/syslog/cpp/macros.h>

#include <utility>

namespace tracing {

TraceManagerApp::TraceManagerApp(std::unique_ptr<sys::ComponentContext> context, Config config,
                                 async::Executor& executor)
    : context_(std::move(context)), trace_manager_(this, std::move(config), executor) {
  [[maybe_unused]] zx_status_t status;

  status = context_->outgoing()->AddPublicService(
      provider_registry_bindings_.GetHandler(&trace_manager_));
  FX_DCHECK(status == ZX_OK);

  status =
      context_->outgoing()->AddPublicService(provisioner_bindings_.GetHandler(&trace_manager_));
  FX_DCHECK(status == ZX_OK);

  FX_LOGS(DEBUG) << "TraceManager services registered";
}

void TraceManagerApp::AddSessionBinding(
    std::shared_ptr<controller::Session> trace_session,
    fidl::InterfaceRequest<controller::Session> session_controller) {
  session_bindings_.AddBinding(trace_session, std::move(session_controller));
  session_bindings_.set_empty_set_handler([this]() { trace_manager_.OnEmptyControllerSet(); });

  FX_LOGS(DEBUG) << "TraceController registered";
}

TraceManagerApp::~TraceManagerApp() = default;

}  // namespace tracing
