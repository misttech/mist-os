// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/trace_manager.h"

#include <fuchsia/tracing/cpp/fidl.h>
#include <lib/fidl/cpp/clone.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <algorithm>
#include <iostream>
#include <unordered_set>

#include "src/performance/trace_manager/app.h"

namespace tracing {
namespace {

// For large traces or when verbosity is on it can take awhile to write out
// all the records. E.g., cpuperf_provider can take 40 seconds with --verbose=2
constexpr zx::duration kStopTimeout = zx::sec(60);
constexpr uint32_t kMinBufferSizeMegabytes = 1;

// These defaults are copied from fuchsia.tracing/trace_controller.fidl.
constexpr uint32_t kDefaultBufferSizeMegabytesHint = 4;
constexpr zx::duration kDefaultStartTimeout{zx::msec(5000)};
constexpr fuchsia::tracing::BufferingMode kDefaultBufferingMode =
    fuchsia::tracing::BufferingMode::ONESHOT;

constexpr size_t kMaxAlertQueueDepth = 16;

uint32_t ConstrainBufferSize(uint32_t buffer_size_megabytes) {
  return std::max(buffer_size_megabytes, kMinBufferSizeMegabytes);
}

struct KnownCategoryHash {
  auto operator()(const fuchsia::tracing::KnownCategory& k) const -> size_t {
    return std::hash<std::string>{}(k.name) ^ std::hash<std::string>{}(k.description);
  }
};

struct KnownCategoryEquals {
  auto operator()(const fuchsia::tracing::KnownCategory& k1,
                  const fuchsia::tracing::KnownCategory& k2) const -> bool {
    return k1.name == k2.name && k1.description == k2.description;
  }
};

using KnownCategorySet =
    std::unordered_set<fuchsia::tracing::KnownCategory, KnownCategoryHash, KnownCategoryEquals>;
using KnownCategoryVector = std::vector<fuchsia::tracing::KnownCategory>;

}  // namespace

TraceController::TraceController(TraceManagerApp* app, std::unique_ptr<TraceSession> session)
    : app_(app), session_(std::move(session)) {
  session_->MarkInitialized();
}

TraceController::~TraceController() = default;

// fidl
void TraceController::handle_unknown_method(uint64_t ordinal, bool method_has_response) {
  FX_LOGS(WARNING) << "Received an unknown method with ordinal " << ordinal;
}

// fidl
void TraceController::StartTracing(controller::StartOptions options,
                                   StartTracingCallback start_callback) {
  FX_LOGS(DEBUG) << "StartTracing";

  controller::Session_StartTracing_Result result;

  if (!session_) {
    FX_LOGS(ERROR) << "Ignoring start request, trace must be initialized first";
    result.set_err(controller::StartErrorCode::NOT_INITIALIZED);
    start_callback(std::move(result));
    return;
  }

  switch (session_->state()) {
    case TraceSession::State::kStarting:
    case TraceSession::State::kStarted:
      FX_LOGS(ERROR) << "Ignoring start request, trace already started";
      result.set_err(controller::StartErrorCode::ALREADY_STARTED);
      start_callback(std::move(result));
      return;
    case TraceSession::State::kStopping:
      FX_LOGS(ERROR) << "Ignoring start request, trace stopping";
      result.set_err(controller::StartErrorCode::STOPPING);
      start_callback(std::move(result));
      return;
    case TraceSession::State::kTerminating:
      FX_LOGS(ERROR) << "Ignoring start request, trace terminating";
      result.set_err(controller::StartErrorCode::TERMINATING);
      start_callback(std::move(result));
      return;
    case TraceSession::State::kInitialized:
    case TraceSession::State::kStopped:
      break;
    default:
      FX_NOTREACHED();
      return;
  }

  std::vector<std::string> additional_categories;
  if (options.has_additional_categories()) {
    additional_categories = std::move(options.additional_categories());
  }

  // This default matches trace's.
  fuchsia::tracing::BufferDisposition buffer_disposition =
      fuchsia::tracing::BufferDisposition::RETAIN;
  if (options.has_buffer_disposition()) {
    buffer_disposition = options.buffer_disposition();
    switch (buffer_disposition) {
      case fuchsia::tracing::BufferDisposition::CLEAR_ENTIRE:
      case fuchsia::tracing::BufferDisposition::CLEAR_NONDURABLE:
      case fuchsia::tracing::BufferDisposition::RETAIN:
        break;
      default:
        FX_LOGS(ERROR) << "Bad value for buffer disposition: " << buffer_disposition
                       << ", dropping connection";
        // TODO(dje): IWBN to drop the connection. How?
        result.set_err(controller::StartErrorCode::TERMINATING);
        start_callback(std::move(result));
        return;
    }
  }

  FX_LOGS(INFO) << "Starting trace, buffer disposition: " << buffer_disposition;

  session_->Start(buffer_disposition, additional_categories, std::move(start_callback));
}

// fidl
void TraceController::StopTracing(controller::StopOptions options,
                                  StopTracingCallback stop_callback) {
  controller::Session_StopTracing_Result stop_result;

  if (session_->state() != TraceSession::State::kInitialized &&
      session_->state() != TraceSession::State::kStarting &&
      session_->state() != TraceSession::State::kStarted) {
    FX_LOGS(INFO) << "Ignoring stop request, state != Initialized,Starting,Started";
    stop_result.set_err(controller::StopErrorCode::NOT_STARTED);
    stop_callback(std::move(stop_result));
    return;
  }

  bool write_results = false;
  if (options.has_write_results()) {
    write_results = options.write_results();
  }

  FX_LOGS(INFO) << "Stopping trace" << (write_results ? ", and writing results" : "");
  session_->Stop(write_results, [stop_callback = std::move(stop_callback)](
                                    controller::Session_StopTracing_Result result) {
    FX_LOGS(DEBUG) << "Stopped trace";
    stop_callback(std::move(result));
  });
}

void TraceController::TerminateTracing(fit::closure cb) {
  // Check the state first because the log messages are useful, but not if
  // tracing has ended.
  if (session_->state() == TraceSession::State::kTerminating) {
    FX_LOGS(INFO) << "Ignoring terminate request. Already terminating";
    return;
  }

  if (write_results_on_terminate_ == false) {
    session_->set_write_results_on_terminate(false);
  }

  session_->Terminate([this, callback = std::move(cb)]() {
    FX_LOGS(DEBUG) << "Session Terminated";

    // Clean up any bindings for currently running trace so a new one
    // can be initiated
    session_.reset();

    FX_DCHECK(callback);
    // The call back will destroy the TraceController object. Only issue the callback
    // as the last thing to do.
    callback();
  });
}

TraceManager::TraceManager(TraceManagerApp* app, Config config, async::Executor& executor)
    : app_(app), config_(std::move(config)), executor_(executor) {}

TraceManager::~TraceManager() = default;

void TraceManager::CloseSession() {
  // Clean up any bindings for the currently running trace so a new one
  // can be initiated.

  // The actual trace_controller object is held by app.SessionBindings
  // and will be removed once the binding is closed. Remove the stored
  // referencce to the trace_controller object to prevent use after free
  FX_LOGS(DEBUG) << "Clean up leftover bindings";
  app_->CloseSessionBindings();
  trace_controller_.reset();
}

void TraceManager::OnEmptyControllerSet() {
  // While one controller could go away and another remain causing a trace
  // to not be terminated, at least handle the common case.
  FX_LOGS(INFO) << "Controller is gone";
  if (trace_controller_) {
    FX_LOGS(DEBUG) << "Terminating trace and closing session";
    // Terminate the running trace and the close the trace session.
    trace_controller_->TerminateTracing([this]() { CloseSession(); });
  } else {
    CloseSession();
  }
}

// fidl
void TraceManager::handle_unknown_method(uint64_t ordinal, bool method_has_response) {
  FX_LOGS(WARNING) << "Received an unknown method with ordinal " << ordinal;
}

// fidl
void TraceManager::InitializeTracing(fidl::InterfaceRequest<controller::Session> controller,
                                     controller::TraceConfig config, zx::socket output) {
  FX_LOGS(DEBUG) << "InitializeTracing";

  if (trace_controller_) {
    FX_LOGS(ERROR) << "Ignoring initialize request, trace already initialized";
    return;
  }

  uint32_t default_buffer_size_megabytes = kDefaultBufferSizeMegabytesHint;
  if (config.has_buffer_size_megabytes_hint()) {
    const uint32_t buffer_size_mb_hint = config.buffer_size_megabytes_hint();
    default_buffer_size_megabytes = ConstrainBufferSize(buffer_size_mb_hint);
  }

  TraceProviderSpecMap provider_specs;
  if (config.has_provider_specs()) {
    for (const auto& it : config.provider_specs()) {
      TraceProviderSpec provider_spec;
      if (it.has_buffer_size_megabytes_hint()) {
        provider_spec.buffer_size_megabytes = it.buffer_size_megabytes_hint();
      }
      if (it.has_categories()) {
        provider_spec.categories = it.categories();
      }
      provider_specs[it.name()] = provider_spec;
    }
  }

  fuchsia::tracing::BufferingMode tracing_buffering_mode = kDefaultBufferingMode;
  if (config.has_buffering_mode()) {
    tracing_buffering_mode = config.buffering_mode();
  }
  const char* mode_name;
  switch (tracing_buffering_mode) {
    case fuchsia::tracing::BufferingMode::ONESHOT:
      mode_name = "oneshot";
      break;
    case fuchsia::tracing::BufferingMode::CIRCULAR:
      mode_name = "circular";
      break;
    case fuchsia::tracing::BufferingMode::STREAMING:
      mode_name = "streaming";
      break;
    default:
      FX_LOGS(ERROR) << "Invalid buffering mode: " << static_cast<unsigned>(tracing_buffering_mode);
      return;
  }

  FX_LOGS(INFO) << "Initializing trace with " << default_buffer_size_megabytes
                << " MB buffers, buffering mode=" << mode_name;
  if (provider_specs.size() > 0) {
    FX_LOGS(INFO) << "Provider overrides:";
    for (const auto& it : provider_specs) {
      FX_LOGS(INFO) << it.first << ": buffer size "
                    << it.second.buffer_size_megabytes.value_or(default_buffer_size_megabytes)
                    << " MB";
    }
  }

  std::vector<std::string> categories;
  if (config.has_categories()) {
    categories = config.categories();
  }

  zx::duration start_timeout = kDefaultStartTimeout;
  if (config.has_start_timeout_milliseconds()) {
    start_timeout = zx::msec(config.start_timeout_milliseconds());
  }

  auto session = std::make_unique<TraceSession>(
      executor_, std::move(output), std::move(categories), default_buffer_size_megabytes,
      tracing_buffering_mode, std::move(provider_specs), start_timeout, kStopTimeout,
      [this]() {
        if (trace_controller_) {
          // We only abort when the write to socket fails. We do not want to attempt
          // to write to the socket again.
          trace_controller_->write_results_on_terminate_ = false;
          trace_controller_->TerminateTracing([this]() { CloseSession(); });
        }
      },
      [this](const std::string& alert_name) { trace_controller_->OnAlert(alert_name); });

  // The trace header is written now to ensure it appears first, and to avoid
  // timing issues if the trace is terminated early (and the session being
  // deleted).
  session->WriteTraceInfo();

  for (auto& bundle : providers_) {
    session->AddProvider(&bundle);
  }

  trace_controller_ = std::make_shared<TraceController>(app_, std::move(session));
  app_->AddSessionBinding(trace_controller_, std::move(controller));
}

// fidl
void TraceManager::GetProviders(GetProvidersCallback callback) {
  FX_LOGS(DEBUG) << "GetProviders";
  controller::Provisioner_GetProviders_Result result;
  std::vector<controller::ProviderInfo> provider_info;
  for (const auto& provider : providers_) {
    controller::ProviderInfo info;
    info.set_id(provider.id);
    info.set_pid(provider.pid);
    info.set_name(provider.name);
    provider_info.push_back(std::move(info));
  }
  result.set_response(controller::Provisioner_GetProviders_Response(std::move(provider_info)));
  callback(std::move(result));
}

// Allows multiple callers to race to call the same callback.
// The first caller will successfully have their value forwarded to the callback, and each
// subsequent call will be dropped. This allows a callback to race against a timeout to call a
// completer.
//
// The CompleterMerger is internally reference counted so that it may be passed by value as a
// callback to multiple callers
template <typename T>
class CompleterMerger {
 public:
  explicit CompleterMerger(fit::function<void(T)> completer)
      : state_(std::make_shared<State>(std::move(completer))) {}

  void operator()(T&& categories) const {
    bool expected = false;
    if (state_->called_.compare_exchange_weak(expected, true)) {
      state_->completer_(std::forward<T>(categories));
    }
  }

 private:
  struct State {
    explicit State(fit::function<void(T)> completer)
        : called_(false), completer_(std::move(completer)) {}
    std::atomic<bool> called_;
    fit::function<void(T)> completer_;
  };
  std::shared_ptr<State> state_;
};

// fidl
void TraceManager::GetKnownCategories(GetKnownCategoriesCallback callback) {
  FX_LOGS(DEBUG) << "GetKnownCategories";
  KnownCategorySet known_categories;
  for (const auto& [name, description] : config_.known_categories()) {
    known_categories.insert({.name = name, .description = description});
  }
  std::vector<fpromise::promise<KnownCategoryVector>> promises;
  fpromise::promise<> timeout = executor_.MakeDelayedPromise(zx::sec(1));
  for (const auto& provider : providers_) {
    fpromise::bridge<KnownCategoryVector> bridge;
    promises.push_back(bridge.consumer.promise());

    CompleterMerger<KnownCategoryVector> merger{bridge.completer.bind()};
    provider.provider->GetKnownCategories(merger);
    timeout = fpromise::promise<>{timeout.and_then([merger = merger]() mutable { merger({}); })};
  }
  auto joined_promise =
      fpromise::join_promise_vector(std::move(promises))
          .and_then(
              [callback = std::move(callback), known_categories = std::move(known_categories)](
                  std::vector<fpromise::result<KnownCategoryVector>>& results) mutable {
                for (const auto& result : results) {
                  if (result.is_ok()) {
                    const auto& result_known_categories = result.value();
                    known_categories.insert(result_known_categories.begin(),
                                            result_known_categories.end());
                  }
                }
                controller::Provisioner_GetKnownCategories_Result result;
                result.set_response(controller::Provisioner_GetKnownCategories_Response(
                    {known_categories.begin(), known_categories.end()}));
                callback(std::move(result));
              });

  executor_.schedule_task(std::move(joined_promise));
  executor_.schedule_task(std::move(timeout));
}

void TraceController::WatchAlert(WatchAlertCallback cb) {
  FX_LOGS(DEBUG) << "WatchAlert";
  if (alerts_.empty()) {
    watch_alert_callbacks_.push(std::move(cb));
  } else {
    controller::Session_WatchAlert_Result result;
    result.set_response(controller::Session_WatchAlert_Response(std::move(alerts_.front())));
    cb(std::move(result));
    alerts_.pop();
  }
}

void TraceManager::RegisterProviderWorker(fidl::InterfaceHandle<provider::Provider> provider,
                                          uint64_t pid, fidl::StringPtr name) {
  FX_LOGS(DEBUG) << "Registering provider {" << pid << ":" << name.value_or("") << "}";
  auto it = providers_.emplace(providers_.end(), provider.Bind(), next_provider_id_++, pid,
                               name.value_or(""));
  it->provider.set_error_handler([this, it](zx_status_t status) {
    if (session()) {
      session()->RemoveDeadProvider(&(*it));
    }
    providers_.erase(it);
  });

  if (session()) {
    session()->AddProvider(&(*it));
  }
}

// fidl
void TraceManager::RegisterProvider(fidl::InterfaceHandle<provider::Provider> provider,
                                    uint64_t pid, std::string name) {
  RegisterProviderWorker(std::move(provider), pid, std::move(name));
}

// fidl
void TraceManager::RegisterProviderSynchronously(fidl::InterfaceHandle<provider::Provider> provider,
                                                 uint64_t pid, std::string name,
                                                 RegisterProviderSynchronouslyCallback callback) {
  RegisterProviderWorker(std::move(provider), pid, std::move(name));
  auto session_ptr = session();
  bool already_started = (session_ptr && (session_ptr->state() == TraceSession::State::kStarting ||
                                          session_ptr->state() == TraceSession::State::kStarted));
  callback(ZX_OK, already_started);
}

void TraceController::SendSessionStateEvent(controller::SessionState state) {
  for (const auto& binding : app_->session_bindings().bindings()) {
    binding->events().OnSessionStateChange(state);
  }
}

TraceSession* TraceManager::session() const {
  if (trace_controller_) {
    return trace_controller_->session();
  }
  return nullptr;
}

controller::SessionState TraceController::TranslateSessionState(TraceSession::State state) {
  switch (state) {
    case TraceSession::State::kReady:
      return controller::SessionState::READY;
    case TraceSession::State::kInitialized:
      return controller::SessionState::INITIALIZED;
    case TraceSession::State::kStarting:
      return controller::SessionState::STARTING;
    case TraceSession::State::kStarted:
      return controller::SessionState::STARTED;
    case TraceSession::State::kStopping:
      return controller::SessionState::STOPPING;
    case TraceSession::State::kStopped:
      return controller::SessionState::STOPPED;
    case TraceSession::State::kTerminating:
      return controller::SessionState::TERMINATING;
  }
}

void TraceController::OnAlert(const std::string& alert_name) {
  if (watch_alert_callbacks_.empty()) {
    if (alerts_.size() == kMaxAlertQueueDepth) {
      // We're at our queue depth limit. Discard the oldest alert.
      alerts_.pop();
    }

    alerts_.push(alert_name);
    return;
  }

  controller::Session_WatchAlert_Result result;
  result.set_response(controller::Session_WatchAlert_Response(alert_name));
  watch_alert_callbacks_.front()(std::move(result));
  watch_alert_callbacks_.pop();
}

}  // namespace tracing
