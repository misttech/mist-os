// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/stop_signals.h"

#include <fuchsia/hardware/power/statecontrol/cpp/fidl.h>
#include <fuchsia/process/lifecycle/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/fidl/cpp/interface_request.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>

#include <memory>

#include "src/developer/forensics/utils/errors.h"

namespace forensics::feedback {
namespace {

// Handles receiving the signal the component should stop and converting into a
// `LifecycleStopSignal`.
class LifecycleServer : public fuchsia::process::lifecycle::Lifecycle {
 public:
  LifecycleServer(async_dispatcher_t* dispatcher,
                  fidl::InterfaceRequest<fuchsia::process::lifecycle::Lifecycle> request,
                  fpromise::completer<LifecycleStopSignal, Error> completer);

  void Stop() override;

 private:
  std::unique_ptr<fidl::Binding<fuchsia::process::lifecycle::Lifecycle>> binding_;
  fpromise::completer<LifecycleStopSignal, Error> completer_;
};

LifecycleServer::LifecycleServer(
    async_dispatcher_t* dispatcher,
    fidl::InterfaceRequest<fuchsia::process::lifecycle::Lifecycle> request,
    fpromise::completer<LifecycleStopSignal, Error> completer)
    : binding_(std::make_unique<fidl::Binding<fuchsia::process::lifecycle::Lifecycle>>(
          this, std::move(request), dispatcher)),
      completer_(std::move(completer)) {
  binding_->set_error_handler([this](const zx_status_t status) {
    if (!completer_) {
      return;
    }

    FX_PLOGS(WARNING, status) << "Lost connection to lifecycle client";
    completer_.complete_error(Error::kConnectionError);
  });
}

void LifecycleServer::Stop() {
  if (completer_) {
    // Move `binding_` to avoid breaking references to internal member variables.
    auto cb = fit::defer([b = std::move(binding_)]() mutable { b->Unbind(); });
    completer_.complete_ok(LifecycleStopSignal([cb = std::move(cb)]() mutable { cb.call(); }));
  }
}

}  // namespace

LifecycleStopSignal::LifecycleStopSignal(fit::callback<void(void)> callback)
    : callback_(std::move(callback)) {
  FX_CHECK(callback_ != nullptr);
}

fpromise::promise<LifecycleStopSignal, Error> WaitForLifecycleStop(
    async_dispatcher_t* dispatcher,
    fidl::InterfaceRequest<fuchsia::process::lifecycle::Lifecycle> request) {
  if (!request.is_valid()) {
    return fpromise::make_result_promise<LifecycleStopSignal, Error>(
        fpromise::error(Error::kBadValue));
  }

  fpromise::bridge<LifecycleStopSignal, Error> bridge;
  auto lifecycle = std::make_unique<LifecycleServer>(dispatcher, std::move(request),
                                                     std::move(bridge.completer));
  return bridge.consumer.promise_or(fpromise::error(Error::kLogicError))
      .then([l = std::move(lifecycle)](fpromise::result<LifecycleStopSignal, Error>& result) {
        return std::move(result);
      });
}

namespace {

// Handles receiving the reason a reboot is expected to occur and converting into a
// `GracefulRebootReasonSignal`.
class RebootWatcherServer : public fuchsia::hardware::power::statecontrol::RebootWatcher {
 public:
  RebootWatcherServer(
      async_dispatcher_t* dispatcher,
      fidl::InterfaceRequest<fuchsia::hardware::power::statecontrol::RebootWatcher> request,
      fpromise::completer<GracefulRebootReasonSignal, Error> completer);

  void OnReboot(fuchsia::hardware::power::statecontrol::RebootOptions options,
                OnRebootCallback callback) override;

 private:
  std::unique_ptr<fidl::Binding<fuchsia::hardware::power::statecontrol::RebootWatcher>> binding_;
  fpromise::completer<GracefulRebootReasonSignal, Error> completer_;
};

RebootWatcherServer::RebootWatcherServer(
    async_dispatcher_t* dispatcher,
    fidl::InterfaceRequest<fuchsia::hardware::power::statecontrol::RebootWatcher> request,
    fpromise::completer<GracefulRebootReasonSignal, Error> completer)
    : binding_(
          std::make_unique<fidl::Binding<fuchsia::hardware::power::statecontrol::RebootWatcher>>(
              this, std::move(request), dispatcher)),
      completer_(std::move(completer)) {
  binding_->set_error_handler([this](const zx_status_t status) {
    if (!completer_) {
      return;
    }

    FX_PLOGS(WARNING, status)
        << "Lost connection to reboot methods watcher client, wont' reconnect";
    completer_.complete_error(Error::kConnectionError);
  });
}

void RebootWatcherServer::OnReboot(fuchsia::hardware::power::statecontrol::RebootOptions options,
                                   OnRebootCallback callback) {
  if (!completer_) {
    callback();
    return;
  }

  // Move `binding_` to avoid breaking references to internal member variables.
  auto cb = fit::defer([cb = std::move(callback), b = std::move(binding_)]() mutable {
    cb();
    b->Unbind();
  });
  completer_.complete_ok(GracefulRebootReasonSignal(ToGracefulRebootReasons(std::move(options)),
                                                    [cb = std::move(cb)]() mutable { cb.call(); }));
}

}  // namespace

GracefulRebootReasonSignal::GracefulRebootReasonSignal(std::vector<GracefulRebootReason> reasons,
                                                       fit::callback<void(void)> callback)
    : reasons_(std::move(reasons)), callback_(std::move(callback)) {
  FX_CHECK(callback_ != nullptr);
}

fpromise::promise<GracefulRebootReasonSignal, Error> WaitForRebootReason(
    async_dispatcher_t* dispatcher,
    fidl::InterfaceRequest<fuchsia::hardware::power::statecontrol::RebootWatcher> request) {
  if (!request.is_valid()) {
    return fpromise::make_result_promise<GracefulRebootReasonSignal, Error>(
        fpromise::error(Error::kBadValue));
  }

  fpromise::bridge<GracefulRebootReasonSignal, Error> bridge;
  auto watcher = std::make_unique<RebootWatcherServer>(dispatcher, std::move(request),
                                                       std::move(bridge.completer));
  return bridge.consumer.promise_or(fpromise::error(Error::kLogicError))
      .then([w = std::move(watcher)](fpromise::result<GracefulRebootReasonSignal, Error>& result) {
        return std::move(result);
      });
}

}  // namespace forensics::feedback
