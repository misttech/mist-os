// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/trace_session.h"

#include <lib/async/cpp/executor.h>
#include <lib/async/default.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-engine/fields.h>

#include <numeric>
#include <unordered_set>

#include "src/performance/trace_manager/util.h"

namespace tracing {

TraceSession::TraceSession(async::Executor& executor, zx::socket destination,
                           std::vector<std::string> enabled_categories,
                           size_t buffer_size_megabytes,
                           fuchsia::tracing::BufferingMode buffering_mode,
                           TraceProviderSpecMap&& provider_specs, zx::duration start_timeout,
                           zx::duration stop_timeout, controller::FxtVersion fxt_version,
                           fit::closure abort_handler, AlertCallback alert_callback)
    : executor_(executor),
      buffer_forwarder_(std::make_shared<BufferForwarder>(std::move(destination))),
      enabled_categories_(std::move(enabled_categories)),
      buffer_size_megabytes_(buffer_size_megabytes),
      buffering_mode_(buffering_mode),
      provider_specs_(std::move(provider_specs)),
      start_timeout_(start_timeout),
      stop_timeout_(stop_timeout),
      fxt_version_(std::move(fxt_version)),
      abort_handler_(std::move(abort_handler)),
      alert_callback_(std::move(alert_callback)),
      weak_ptr_factory_(this) {}

TraceSession::~TraceSession() {
  session_start_timeout_.Cancel();
  session_stop_timeout_.Cancel();
  session_terminate_timeout_.Cancel();
}

void TraceSession::AddProvider(TraceProviderBundle* provider) {
  if (state_ == State::kTerminating) {
    FX_LOGS(DEBUG) << "Ignoring new provider " << *provider << ", terminating";
    return;
  }

  size_t buffer_size_megabytes = buffer_size_megabytes_;
  // Include at least the umbrella enabled categories.
  std::unordered_set<std::string> provider_specific_categories(enabled_categories_->begin(),
                                                               enabled_categories_->end());
  auto spec_iter = provider_specs_.find(provider->name);
  if (spec_iter != provider_specs_.end()) {
    const TraceProviderSpec* spec = &spec_iter->second;
    if (spec->buffer_size_megabytes) {
      buffer_size_megabytes = spec->buffer_size_megabytes.value();
    }
    provider_specific_categories.insert(spec->categories.begin(), spec->categories.end());
  }
  uint64_t buffer_size = buffer_size_megabytes * 1024 * 1024;

  FX_LOGS(DEBUG) << "Adding provider " << *provider << ", buffer size " << buffer_size_megabytes
                 << "MB";

  tracees_.emplace_back(std::make_unique<Tracee>(executor_, buffer_forwarder_, provider));
  std::vector<std::string> categories_clone(provider_specific_categories.begin(),
                                            provider_specific_categories.end());
  if (!tracees_.back()->Initialize(
          std::move(categories_clone), buffer_size, buffering_mode_,
          [weak = weak_ptr_factory_.GetWeakPtr(), provider]() {
            if (weak) {
              weak->OnProviderStarted(provider);
            }
          },
          [weak = weak_ptr_factory_.GetWeakPtr(), provider](bool write_results) {
            if (weak) {
              weak->OnProviderStopped(provider, write_results);
            }
          },
          [weak = weak_ptr_factory_.GetWeakPtr(), provider]() {
            if (weak) {
              weak->OnProviderTerminated(provider);
            }
          },
          [weak = weak_ptr_factory_.GetWeakPtr()](const std::string& alert_name) {
            if (weak && weak->alert_callback_) {
              weak->alert_callback_(alert_name);
            }
          })) {
    tracees_.pop_back();
  } else {
    Tracee* tracee = tracees_.back().get();
    switch (state_) {
      case State::kReady:
      case State::kInitialized:
        // Nothing more to do.
        break;
      case State::kStarting:
      case State::kStarted:
        // This is a new provider, there is nothing in the buffer to retain.
        tracee->Start(fuchsia::tracing::BufferDisposition::CLEAR_ENTIRE, additional_categories_);
        break;
      case State::kStopping:
      case State::kStopped:
        // Mark the tracee as stopped so we don't try to wait for it to do so.
        // This is a new provider, there are no results to write.
        tracee->Stop(/*write_results=*/false);
        break;
      default:
        FX_NOTREACHED();
        break;
    }
  }
}

void TraceSession::MarkInitialized() { TransitionToState(State::kInitialized); }

void TraceSession::Terminate(fit::closure callback) {
  if (state_ == State::kTerminating) {
    FX_LOGS(DEBUG) << "Ignoring terminate request, already terminating";
    return;
  }

  TransitionToState(State::kTerminating);
  terminate_callback_ = std::move(callback);

  for (const auto& tracee : tracees_) {
    tracee->Terminate();
  }

  session_terminate_timeout_.PostDelayed(executor_.dispatcher(), stop_timeout_);
  TerminateSessionIfEmpty();
}

void TraceSession::Start(fuchsia::tracing::BufferDisposition buffer_disposition,
                         const std::vector<std::string>& additional_categories,
                         controller::Session::StartTracingCallback callback) {
  FX_DCHECK(state_ == State::kInitialized || state_ == State::kStopped);

  if (force_clear_buffer_contents_) {
    // "force-clear" -> Clear the entire buffer because it was saved.
    buffer_disposition = fuchsia::tracing::BufferDisposition::CLEAR_ENTIRE;
  }
  force_clear_buffer_contents_ = false;

  for (const auto& tracee : tracees_) {
    tracee->Start(buffer_disposition, additional_categories);
  }

  start_callback_ = std::move(callback);
  session_start_timeout_.PostDelayed(executor_.dispatcher(), start_timeout_);

  // Clear out any old trace stats before starting a new session.
  trace_stats_.clear();

  // We haven't fully started at this point, we still have to wait for each
  // provider to indicate it they've started.
  TransitionToState(State::kStarting);

  // If there are no providers currently registered, then we are started.
  CheckAllProvidersStarted();

  // Save for tracees that come along later.
  additional_categories_ = additional_categories;
}

void TraceSession::Stop(bool write_results,
                        fit::function<void(controller::Session_StopTracing_Result)> callback) {
  FX_DCHECK(state_ == State::kInitialized || state_ == State::kStarting ||
            state_ == State::kStarted);

  TransitionToState(State::kStopping);
  stop_callback_ = std::move(callback);

  for (const auto& tracee : tracees_) {
    tracee->Stop(write_results);
  }

  // If we're writing results then force-clear the buffer on the next Start.
  if (write_results) {
    force_clear_buffer_contents_ = true;
  }

  session_stop_timeout_.PostDelayed(executor_.dispatcher(), stop_timeout_);
  CheckAllProvidersStopped();

  // Clear out, must be respecified for each Start() request.
  additional_categories_.clear();
}

// Called when a provider reports that it has started.

void TraceSession::OnProviderStarted(TraceProviderBundle* bundle) {
  if (state_ == State::kStarting) {
    CheckAllProvidersStarted();
  } else if (state_ == State::kStarted) {
    // Nothing to do. One example of when this can happen is if we time out
    // waiting for providers to start and then a provider reports starting
    // afterwards.
  } else {
    // Tracing likely stopped or terminated in the interim.
    auto it = std::find_if(tracees_.begin(), tracees_.end(),
                           [bundle](const auto& tracee) { return *tracee == bundle; });

    if (it != tracees_.end()) {
      if (state_ == State::kReady || state_ == State::kInitialized) {
        FX_LOGS(WARNING) << "Provider " << *bundle << " sent a \"started\""
                         << " notification but tracing hasn't started";
        // Misbehaving provider, but it may just be slow.
        (*it)->Stop(/*write_results=*/false);
      } else if (state_ == State::kStopping || state_ == State::kStopped) {
        (*it)->Stop(/*write_results=*/false);
      } else {
        (*it)->Terminate();
      }
    }
  }
}

// Called when a provider state change is detected.
// This includes "failed" as well as "started".

void TraceSession::CheckAllProvidersStarted() {
  FX_DCHECK(state_ == State::kStarting);

  bool all_started =
      std::accumulate(tracees_.begin(), tracees_.end(), true, [](bool value, const auto& tracee) {
        bool ready = (tracee->state() == Tracee::State::kStarted ||
                      // If a provider fails to start continue tracing.
                      // TODO(https://fxbug.dev/42096895): We should still record what providers
                      // failed to start (but is that done in timeout handling?).
                      tracee->state() == Tracee::State::kStopped);
        FX_LOGS(DEBUG) << "tracee " << *tracee->bundle() << (ready ? "" : " not") << " ready";
        return value && ready;
      });

  if (all_started) {
    FX_LOGS(DEBUG) << "All providers reporting started";
    NotifyStarted();
  }
}

void TraceSession::NotifyStarted() {
  TransitionToState(State::kStarted);
  if (start_callback_) {
    FX_LOGS(DEBUG) << "Marking session as having started";
    session_start_timeout_.Cancel();
    auto callback = std::move(start_callback_);
    controller::Session_StartTracing_Result result;
    controller::Session_StartTracing_Response response;
    result.set_response(response);
    callback(std::move(result));
  }
}

void TraceSession::FinishStartingDueToTimeout() { NotifyStarted(); }

void TraceSession::OnProviderStopped(TraceProviderBundle* bundle, bool write_results) {
  auto it = std::find_if(tracees_.begin(), tracees_.end(),
                         [bundle](const auto& tracee) { return *tracee == bundle; });

  if (write_results) {
    if (it != tracees_.end()) {
      Tracee* tracee = (*it).get();
      if (!tracee->results_written()) {
        if (!WriteProviderData(tracee)) {
          Abort();
          return;
        }
      }
    }
  }

  if (state_ == State::kStopped) {
    // Late stop notification, nothing more to do.
  } else if (state_ == State::kStopping) {
    CheckAllProvidersStopped();
  } else {
    // Tracing may have terminated in the interim.
    if (it != tracees_.end()) {
      if (state_ == State::kTerminating) {
        (*it)->Terminate();
      }
    }
  }
}

void TraceSession::CheckAllProvidersStopped() {
  FX_DCHECK(state_ == State::kStopping);

  bool all_stopped =
      std::accumulate(tracees_.begin(), tracees_.end(), true, [](bool value, const auto& tracee) {
        bool stopped = tracee->state() == Tracee::State::kStopped;
        FX_LOGS(DEBUG) << "tracee " << *tracee->bundle() << (stopped ? "" : " not") << " stopped";
        return value && stopped;
      });

  if (all_stopped) {
    FX_LOGS(DEBUG) << "All providers reporting stopped";
    TransitionToState(State::kStopped);
    NotifyStopped();
  }
}

void TraceSession::NotifyStopped() {
  if (stop_callback_) {
    FX_LOGS(DEBUG) << "Marking session as having stopped";
    session_stop_timeout_.Cancel();
    for (auto& tracee : tracees_) {
      if (auto trace_stat = tracee->GetStats(); trace_stat.has_value()) {
        trace_stats_.push_back(std::move(trace_stat.value()));
      } else {
        FX_LOGS(WARNING) << "No stats generated for " << tracee->bundle();
      }
    }

    controller::Session_StopTracing_Result stop_result;
    controller::StopResult result;
    result.set_provider_stats(std::move(trace_stats_));
    stop_result.set_response(std::move(result));
    auto callback = std::move(stop_callback_);
    FX_DCHECK(callback);
    callback(std::move(stop_result));
  }
}

void TraceSession::FinishStoppingDueToTimeout() {
  if (state_ == State::kStopping) {
    FX_LOGS(DEBUG) << "Marking session as stopped, timed out waiting for tracee(s)";
    TransitionToState(State::kStopped);
    for (auto& tracee : tracees_) {
      if (tracee->state() != Tracee::State::kStopped)
        FX_LOGS(WARNING) << "Timed out waiting for trace provider " << *tracee->bundle()
                         << " to stop";
    }
    NotifyStopped();
  }
}

void TraceSession::OnProviderTerminated(TraceProviderBundle* bundle) {
  auto it = std::find_if(tracees_.begin(), tracees_.end(),
                         [bundle](const auto& tracee) { return *tracee == bundle; });

  if (it != tracees_.end()) {
    if (write_results_on_terminate_) {
      Tracee* tracee = (*it).get();
      // If the last Stop request saved the results, don't save them again.
      // But don't write results if the tracee was never started.
      if (tracee->was_started() && !tracee->results_written()) {
        if (!WriteProviderData(tracee)) {
          Abort();
          return;
        }
      }
    }
    tracees_.erase(it);
  }

  if (state_ == State::kStarting) {
    // A trace provider may have disconnected without having first successfully
    // started. Check whether all remaining providers have now started so that
    // we can transition to |kStarted|.
    CheckAllProvidersStarted();
  } else if (state_ == State::kStopping) {
    // A trace provider may have disconnected without having been marked as
    // stopped. Check whether all remaining providers have now stopped.
    CheckAllProvidersStopped();
  }

  TerminateSessionIfEmpty();
}

void TraceSession::TerminateSessionIfEmpty() {
  if (state_ == State::kTerminating && tracees_.empty()) {
    FX_LOGS(DEBUG) << "Marking session as terminated, no more tracees";

    session_terminate_timeout_.Cancel();
    auto callback = std::move(terminate_callback_);
    FX_DCHECK(callback);
    callback();
  }
}

void TraceSession::FinishTerminatingDueToTimeout() {
  // We do not consider pending_start_tracees_ here as we only
  // terminate them as a best effort.
  if (state_ == State::kTerminating && !tracees_.empty()) {
    FX_LOGS(DEBUG) << "Marking session as terminated, timed out waiting for tracee(s)";

    for (auto& tracee : tracees_) {
      if (tracee->state() != Tracee::State::kTerminated) {
        FX_LOGS(WARNING) << "Timed out waiting for trace provider " << tracee->bundle()
                         << " to terminate";
      }
    }
    auto callback = std::move(terminate_callback_);
    FX_DCHECK(callback);
    callback();
  }
}

void TraceSession::SessionStartTimeout(async_dispatcher_t* dispatcher, async::TaskBase* task,
                                       zx_status_t status) {
  FX_LOGS(WARNING) << "Waiting for start timed out.";
  FinishStartingDueToTimeout();
}

void TraceSession::SessionStopTimeout(async_dispatcher_t* dispatcher, async::TaskBase* task,
                                      zx_status_t status) {
  FX_LOGS(WARNING) << "Waiting for stop timed out.";
  FinishStoppingDueToTimeout();
}

void TraceSession::SessionTerminateTimeout(async_dispatcher_t* dispatcher, async::TaskBase* task,
                                           zx_status_t status) {
  FX_LOGS(WARNING) << "Waiting for termination timed out.";
  FinishTerminatingDueToTimeout();
}

void TraceSession::RemoveDeadProvider(TraceProviderBundle* bundle) {
  if (state_ == State::kReady) {
    // Session never got started. Nothing to do.
    return;
  }
  OnProviderTerminated(bundle);
}

bool TraceSession::WriteProviderData(Tracee* tracee) {
  FX_DCHECK(!tracee->results_written());

  switch (tracee->TransferRecords()) {
    case TransferStatus::kComplete:
      break;
    case TransferStatus::kProviderError:
      FX_LOGS(ERROR) << "Problem reading provider socket output, skipping";
      break;
    case TransferStatus::kWriteError:
      FX_LOGS(ERROR) << "Encountered unrecoverable error writing socket";
      return false;
    case TransferStatus::kReceiverDead:
      FX_LOGS(ERROR) << "Consumer socket peer is closed";
      return false;
    default:
      __UNREACHABLE;
      break;
  }

  return true;
}

void TraceSession::Abort() {
  FX_LOGS(DEBUG) << "Fatal error occurred, aborting session";
  write_results_on_terminate_ = false;
  if (stop_callback_) {
    TransitionToState(State::kStopped);
    session_stop_timeout_.Cancel();
    controller::Session_StopTracing_Result stop_result;
    stop_result.set_err(controller::StopError::ABORTED);
    auto callback = std::move(stop_callback_);
    FX_DCHECK(callback);
    callback(std::move(stop_result));
  }
  abort_handler_();
}

void TraceSession::WriteTraceInfo() {
  // This won't block as we're only called after the consumer connects, and
  // this is the first record written.
  if (auto status = buffer_forwarder_->WriteMagicNumberRecord();
      status != TransferStatus::kComplete) {
    FX_LOGS(ERROR) << "Failed to write magic number record: " << status;
  }
}

void TraceSession::TransitionToState(State new_state) {
  FX_LOGS(DEBUG) << "Transitioning from " << state_ << " to " << new_state;
  state_ = new_state;
}

std::ostream& operator<<(std::ostream& out, TraceSession::State state) {
  switch (state) {
    case TraceSession::State::kReady:
      out << "ready";
      break;
    case TraceSession::State::kInitialized:
      out << "initialized";
      break;
    case TraceSession::State::kStarting:
      out << "starting";
      break;
    case TraceSession::State::kStarted:
      out << "started";
      break;
    case TraceSession::State::kStopping:
      out << "stopping";
      break;
    case TraceSession::State::kStopped:
      out << "stopped";
      break;
    case TraceSession::State::kTerminating:
      out << "terminating";
      break;
  }

  return out;
}

}  // namespace tracing
