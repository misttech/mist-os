// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/power/testing/fake-suspend/control_server.h"

#include <zircon/errors.h>

#include <utility>

namespace fake_suspend {

using test_suspendcontrol::DeviceAwaitSuspendResponse;

ControlServer::ControlServer(
    std::shared_ptr<std::vector<fuchsia_hardware_suspend::SuspendState>> suspend_states)
    : suspend_states_(std::move(suspend_states)) {}

void ControlServer::SetSuspendStates(SetSuspendStatesRequest& request,
                                     SetSuspendStatesCompleter::Sync& completer) {
  *suspend_states_ = request.suspend_states().value();
  completer.Reply(zx::ok());
}

void ControlServer::AwaitSuspend(AwaitSuspendCompleter::Sync& completer) {
  if (auto resumable = resumable_.lock()) {
    if (resumable->IsSuspended()) {
      completer.Reply(
          zx::ok(DeviceAwaitSuspendResponse().state_index(resumable->LastStateIndex())));
      return;
    }
  }

  await_suspend_completer_ = completer.ToAsync();
}

void ControlServer::Resume(ResumeRequest& request, ResumeCompleter::Sync& completer) {
  if (auto resumable = resumable_.lock()) {
    if (!resumable->IsSuspended()) {
      completer.Reply(zx::error(ZX_ERR_BAD_STATE));
      return;
    }

    completer.Reply(resumable->Resume(request));
    return;
  }

  completer.Reply(zx::ok());
}

void ControlServer::OnSuspend(std::optional<uint64_t> state_index) {
  if (!await_suspend_completer_) {
    return;
  }

  await_suspend_completer_->Reply(zx::ok(DeviceAwaitSuspendResponse().state_index(state_index)));
  await_suspend_completer_.reset();
}

void ControlServer::Serve(async_dispatcher_t* dispatcher,
                          fidl::ServerEnd<test_suspendcontrol::Device> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_suspend
