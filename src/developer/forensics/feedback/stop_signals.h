// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_FEEDBACK_STOP_SIGNALS_H_
#define SRC_DEVELOPER_FORENSICS_FEEDBACK_STOP_SIGNALS_H_

#include <fuchsia/hardware/power/statecontrol/cpp/fidl.h>
#include <fuchsia/process/lifecycle/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/interface_request.h>
#include <lib/fit/function.h>
#include <lib/fpromise/promise.h>
#include <lib/zx/channel.h>

#include "src/developer/forensics/feedback/reboot_log/graceful_reboot_reason.h"
#include "src/developer/forensics/utils/errors.h"

namespace forensics::feedback {

// Indicates `fuchsia.process.lifecycle/Lifecycle.Stop` has been called and provides a way to
// sends a response to the server.
class LifecycleStopSignal {
 public:
  explicit LifecycleStopSignal(fit::callback<void(void)> callback);

  void Respond() { callback_(); }

 private:
  fit::callback<void(void)> callback_;
};

// Indicates `fuchsia.hardware.power.statecontrol/RebootMethodsWatcher.OnReboot` has been called and
// provides a way to get the reason and send a response to the server.
class GracefulRebootReasonSignal {
 public:
  GracefulRebootReasonSignal(GracefulRebootReason reason, fit::callback<void(void)> callback);

  GracefulRebootReason Reason() const { return reason_; }
  void Respond() { callback_(); }

 private:
  GracefulRebootReason reason_;
  fit::callback<void(void)> callback_;
};

// Returns a promise which will complete successfully when the lifecycle signal is received.
//
// Note, the response will be sent when the `LifecycleStopSignal` object is destroyed, if it hasn't
// already been sent.
fpromise::promise<LifecycleStopSignal, Error> WaitForLifecycleStop(
    async_dispatcher_t* dispatcher,
    fidl::InterfaceRequest<fuchsia::process::lifecycle::Lifecycle> request);

// Returns a promise which will complete successfully when the reboot reason signal is received.
//
// Note, the response will be sent when the `GracefulRebootReasonSignal` object is destroyed, if it
// hasn't already been sent.
fpromise::promise<GracefulRebootReasonSignal, Error> WaitForRebootReason(
    async_dispatcher_t* dispatcher,
    fidl::InterfaceRequest<fuchsia::hardware::power::statecontrol::RebootWatcher> request);

}  // namespace forensics::feedback

#endif  // SRC_DEVELOPER_FORENSICS_FEEDBACK_STOP_SIGNALS_H_
