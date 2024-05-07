// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/power/testing/fake-suspend/device_server.h"

#include <optional>
#include <utility>

namespace fake_suspend {

using fuchsia_hardware_suspend::SuspenderGetSuspendStatesResponse;
using fuchsia_hardware_suspend::SuspenderSuspendResponse;

DeviceServer::DeviceServer(
    std::shared_ptr<std::vector<fuchsia_hardware_suspend::SuspendState>> suspend_states)
    : suspend_states_(std::move(suspend_states)) {}

void DeviceServer::GetSuspendStates(GetSuspendStatesCompleter::Sync& completer) {
  completer.Reply(zx::ok(SuspenderGetSuspendStatesResponse().suspend_states(*suspend_states_)));
}

void DeviceServer::Suspend(SuspendRequest& request, SuspendCompleter::Sync& completer) {
  if (request.state_index() >= suspend_states_->size()) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  last_state_index_ = request.state_index();
  suspend_completer_ = completer.ToAsync();
  if (auto suspend_observer = suspend_observer_.lock()) {
    suspend_observer->OnSuspend(request.state_index());
  }
}

zx::result<> DeviceServer::Resume(const DeviceResumeRequest& request) {
  if (!suspend_completer_) {
    return zx::ok();
  }

  if (request.Which() == DeviceResumeRequest::Tag::kResult) {
    suspend_completer_->Reply(zx::ok(SuspenderSuspendResponse()
                                         .reason(request.result()->reason())
                                         .suspend_duration(request.result()->suspend_duration())
                                         .suspend_overhead(request.result()->suspend_overhead())));
  } else {
    suspend_completer_->Reply(zx::error(request.error().value()));
  }
  suspend_completer_.reset();
  return zx::ok();
}

void DeviceServer::Serve(async_dispatcher_t* dispatcher,
                         fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_suspend
