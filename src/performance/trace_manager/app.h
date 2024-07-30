// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_MANAGER_APP_H_
#define SRC_PERFORMANCE_TRACE_MANAGER_APP_H_

#include <fuchsia/tracing/controller/cpp/fidl.h>
#include <fuchsia/tracing/provider/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>

#include <memory>

#include "src/performance/trace_manager/trace_manager.h"

namespace tracing {

class TraceManagerApp {
 public:
  TraceManagerApp(std::unique_ptr<sys::ComponentContext> context, Config config,
                  async::Executor& executor);
  ~TraceManagerApp();

  void AddSessionBinding(std::shared_ptr<controller::Session> trace_session,
                         fidl::InterfaceRequest<controller::Session> session_controller);

  void CloseSessionBindings() { session_bindings_.CloseAll(); }

  // For testing.
  sys::ComponentContext* context() const { return context_.get(); }
  const TraceManager* trace_manager() const { return &trace_manager_; }
  const OldTraceManager* old_trace_manager() const { return &old_trace_manager_; }

  const fidl::BindingSet<controller::Controller>& controller_bindings() const {
    return old_controller_bindings_;
  }

  const fidl::BindingSet<controller::Session, std::shared_ptr<controller::Session>>&
  session_bindings() const {
    return session_bindings_;
  }

 private:
  std::unique_ptr<sys::ComponentContext> context_;

  TraceManager trace_manager_;

  // TODO(b/42083286): Remove after the old trace Controller protocol is removed
  // This is present to maintain backwards comparitibility during the process of
  // migrating to use the new trace protocols. This will be removed once all tracing
  // clients are migrated.
  OldTraceManager old_trace_manager_;

  fidl::BindingSet<fuchsia::tracing::provider::Registry> provider_registry_bindings_;
  fidl::BindingSet<fuchsia::tracing::controller::Controller> old_controller_bindings_;
  fidl::BindingSet<fuchsia::tracing::controller::Provisioner> provisioner_bindings_;
  fidl::BindingSet<fuchsia::tracing::controller::Session,
                   std::shared_ptr<fuchsia::tracing::controller::Session>>
      session_bindings_;

  TraceManagerApp(const TraceManagerApp&) = delete;
  TraceManagerApp(TraceManagerApp&&) = delete;
  TraceManagerApp& operator=(const TraceManagerApp&) = delete;
  TraceManagerApp& operator=(TraceManagerApp&&) = delete;
};

}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_MANAGER_APP_H_
