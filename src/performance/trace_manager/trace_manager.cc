// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/trace_manager.h"

#include <fuchsia/tracing/cpp/fidl.h>
#include <lib/fidl/cpp/clone.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
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
constexpr uint32_t kMaxBufferSizeMegabytes = 64;

// These defaults are copied from fuchsia.tracing/trace_controller.fidl.
constexpr uint32_t kDefaultBufferSizeMegabytesHint = 4;
constexpr uint32_t kDefaultStartTimeoutMilliseconds = 5000;
constexpr fuchsia::tracing::BufferingMode kDefaultBufferingMode =
    fuchsia::tracing::BufferingMode::ONESHOT;

constexpr size_t kMaxAlertQueueDepth = 16;

uint32_t ConstrainBufferSize(uint32_t buffer_size_megabytes) {
  return std::min(std::max(buffer_size_megabytes, kMinBufferSizeMegabytes),
                  kMaxBufferSizeMegabytes);
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

TraceManager::TraceManager(TraceManagerApp* app, Config config, async_dispatcher_t* dispatcher)
    : app_(app), config_(std::move(config)), executor_(dispatcher) {}

TraceManager::~TraceManager() = default;

void TraceManager::OnEmptyControllerSet() {
  // While one controller could go away and another remain causing a trace
  // to not be terminated, at least handle the common case.
  FX_VLOGS(5) << "Controller is gone";
  if (session_) {
    // Check the state first because the log messages are useful, but not if
    // tracing has ended.
    if (session_->state() != TraceSession::State::kTerminating) {
      FX_LOGS(INFO) << "Controller is gone, terminating trace";
      session_->Terminate([this]() {
        FX_LOGS(INFO) << "Trace terminated";
        session_ = nullptr;
      });
    }
  }

  while (!watch_alert_callbacks_.empty()) {
    watch_alert_callbacks_.pop();
  }
  while (!alerts_.empty()) {
    alerts_.pop();
  }
}

// fidl
void TraceManager::InitializeTracing(controller::TraceConfig config, zx::socket output) {
  FX_VLOGS(2) << "InitializeTracing";

  if (session_) {
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
    categories = std::move(config.categories());
  }

  uint64_t start_timeout_milliseconds = kDefaultStartTimeoutMilliseconds;
  if (config.has_start_timeout_milliseconds()) {
    start_timeout_milliseconds = config.start_timeout_milliseconds();
  }

  session_ = fxl::MakeRefCounted<TraceSession>(
      std::move(output), std::move(categories), default_buffer_size_megabytes,
      tracing_buffering_mode, std::move(provider_specs), zx::msec(start_timeout_milliseconds),
      kStopTimeout, [this]() { session_ = nullptr; },
      [this](const std::string& alert_name) { OnAlert(alert_name); });

  // The trace header is written now to ensure it appears first, and to avoid
  // timing issues if the trace is terminated early (and the session being
  // deleted).
  session_->WriteTraceInfo();

  for (auto& bundle : providers_) {
    session_->AddProvider(&bundle);
  }

  session_->MarkInitialized();
}

// fidl
void TraceManager::TerminateTracing(controller::TerminateOptions options,
                                    TerminateTracingCallback terminate_callback) {
  if (!session_) {
    FX_VLOGS(1) << "Ignoring terminate request, tracing not initialized";
    controller::TerminateResult result;
    terminate_callback(std::move(result));
    return;
  }

  if (options.has_write_results()) {
    session_->set_write_results_on_terminate(options.write_results());
  }

  FX_LOGS(INFO) << "Terminating trace";
  session_->Terminate([this, terminate_callback = std::move(terminate_callback)]() {
    FX_LOGS(INFO) << "Terminated trace";
    controller::TerminateResult result;
    // TODO(dje): Report stats back to user.
    terminate_callback(std::move(result));
    session_ = nullptr;
  });
}

// fidl
void TraceManager::StartTracing(controller::StartOptions options,
                                StartTracingCallback start_callback) {
  FX_VLOGS(2) << "StartTracing";

  controller::Controller_StartTracing_Result result;

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
void TraceManager::StopTracing(controller::StopOptions options, StopTracingCallback stop_callback) {
  if (!session_) {
    FX_VLOGS(1) << "Ignoring stop request, tracing not started";
    stop_callback();
    return;
  }

  if (session_->state() != TraceSession::State::kInitialized &&
      session_->state() != TraceSession::State::kStarting &&
      session_->state() != TraceSession::State::kStarted) {
    FX_VLOGS(1) << "Ignoring stop request, state != Initialized,Starting,Started";
    stop_callback();
    return;
  }

  bool write_results = false;
  if (options.has_write_results()) {
    write_results = options.write_results();
  }

  FX_LOGS(INFO) << "Stopping trace" << (write_results ? ", and writing results" : "");
  session_->Stop(write_results, [stop_callback = std::move(stop_callback)]() {
    FX_LOGS(INFO) << "Stopped trace";
    stop_callback();
  });
}

// fidl
void TraceManager::GetProviders(GetProvidersCallback callback) {
  FX_VLOGS(2) << "GetProviders";
  std::vector<controller::ProviderInfo> provider_info;
  for (const auto& provider : providers_) {
    controller::ProviderInfo info;
    info.set_id(provider.id);
    info.set_pid(provider.pid);
    info.set_name(provider.name);
    provider_info.push_back(std::move(info));
  }
  callback(std::move(provider_info));
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
  FX_VLOGS(2) << "GetKnownCategories";
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
                callback({known_categories.begin(), known_categories.end()});
              });

  executor_.schedule_task(std::move(joined_promise));
  executor_.schedule_task(std::move(timeout));
}

void TraceManager::WatchAlert(WatchAlertCallback cb) {
  FX_VLOGS(2) << "WatchAlert";
  if (alerts_.empty()) {
    watch_alert_callbacks_.push(std::move(cb));
  } else {
    cb(std::move(alerts_.front()));
    alerts_.pop();
  }
}

void TraceManager::RegisterProviderWorker(fidl::InterfaceHandle<provider::Provider> provider,
                                          uint64_t pid, fidl::StringPtr name) {
  FX_VLOGS(2) << "Registering provider {" << pid << ":" << name.value_or("") << "}";
  auto it = providers_.emplace(providers_.end(), provider.Bind(), next_provider_id_++, pid,
                               name.value_or(""));

  it->provider.set_error_handler([this, it](zx_status_t status) {
    if (session_)
      session_->RemoveDeadProvider(&(*it));
    providers_.erase(it);
  });

  if (session_) {
    session_->AddProvider(&(*it));
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
  bool already_started = (session_ && (session_->state() == TraceSession::State::kStarting ||
                                       session_->state() == TraceSession::State::kStarted));
  callback(ZX_OK, already_started);
}

void TraceManager::SendSessionStateEvent(controller::SessionState state) {
  for (const auto& binding : app_->controller_bindings().bindings()) {
    binding->events().OnSessionStateChange(state);
  }
}

controller::SessionState TraceManager::TranslateSessionState(TraceSession::State state) {
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

void TraceManager::OnAlert(const std::string& alert_name) {
  if (watch_alert_callbacks_.empty()) {
    if (alerts_.size() == kMaxAlertQueueDepth) {
      // We're at our queue depth limit. Discard the oldest alert.
      alerts_.pop();
    }

    alerts_.push(alert_name);
    return;
  }

  watch_alert_callbacks_.front()(alert_name);
  watch_alert_callbacks_.pop();
}

}  // namespace tracing
