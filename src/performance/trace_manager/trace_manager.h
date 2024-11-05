// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_MANAGER_TRACE_MANAGER_H_
#define SRC_PERFORMANCE_TRACE_MANAGER_TRACE_MANAGER_H_

#include <fuchsia/tracing/controller/cpp/fidl.h>
#include <fuchsia/tracing/cpp/fidl.h>
#include <fuchsia/tracing/provider/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fidl/cpp/interface_ptr_set.h>
#include <lib/fidl/cpp/interface_request.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/zx/socket.h>

#include <list>
#include <queue>

#include "src/performance/trace_manager/config.h"
#include "src/performance/trace_manager/trace_provider_bundle.h"
#include "src/performance/trace_manager/trace_session.h"

namespace tracing {

namespace controller = fuchsia::tracing::controller;
namespace provider = fuchsia::tracing::provider;

// forward decl, here to break mutual header dependency
class TraceManagerApp;
class TraceManager;

class TraceController : public controller::Session {
  friend TraceManager;

 public:
  TraceController(TraceManagerApp* app, std::unique_ptr<TraceSession> session);
  ~TraceController() override;

  void TerminateTracing(fit::closure cb);

  void OnAlert(const std::string& alert_name);

  // For testing.
  TraceSession* session() const { return session_.get(); }

 private:
  // |Session| implementation.
  void StartTracing(controller::StartOptions options, StartTracingCallback cb) override;
  void StopTracing(controller::StopOptions options, StopTracingCallback cb) override;
  void WatchAlert(WatchAlertCallback cb) override;
  void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

  void SendSessionStateEvent(controller::SessionState state);
  controller::SessionState TranslateSessionState(TraceSession::State state);

  TraceManagerApp* const app_;

  // We only set this to false when aborting.
  bool write_results_on_terminate_ = true;

  std::unique_ptr<TraceSession> session_;
  std::queue<std::string> alerts_;
  std::queue<WatchAlertCallback> watch_alert_callbacks_;
};

class TraceManager : public controller::Provisioner, public provider::Registry {
  friend TraceController;

 public:
  TraceManager(TraceManagerApp* app, Config config, async::Executor& executor);
  ~TraceManager() override;

  // For testing.
  TraceSession* session() const;

  void OnEmptyControllerSet();

 private:
  // |Provisioner| implementation.
  void InitializeTracing(fidl::InterfaceRequest<controller::Session> controller,
                         controller::TraceConfig config, zx::socket output) override;
  void GetProviders(GetProvidersCallback cb) override;
  void GetKnownCategories(GetKnownCategoriesCallback callback) override;
  void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

  // |TraceRegistry| implementation.
  void RegisterProviderWorker(fidl::InterfaceHandle<provider::Provider> provider, uint64_t pid,
                              fidl::StringPtr name);
  void RegisterProvider(fidl::InterfaceHandle<provider::Provider> provider, uint64_t pid,
                        std::string name) override;
  void RegisterProviderSynchronously(fidl::InterfaceHandle<provider::Provider> provider,
                                     uint64_t pid, std::string name,
                                     RegisterProviderSynchronouslyCallback callback) override;

  void CloseSession();

  TraceManagerApp* const app_;

  const Config config_;

  std::shared_ptr<TraceController> trace_controller_;
  uint32_t next_provider_id_ = 1u;
  std::list<TraceProviderBundle> providers_;
  async::Executor& executor_;

  TraceManager(const TraceManager&) = delete;
  TraceManager(TraceManager&&) = delete;
  TraceManager& operator=(const TraceManager&) = delete;
  TraceManager& operator=(TraceManager&&) = delete;
};

}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_MANAGER_TRACE_MANAGER_H_
