// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/system_activity_governor.h"

#include <lib/async/cpp/wait.h>
#include <lib/zx/event.h>

#include <utility>

#include "src/lib/testing/predicates/status.h"

namespace forensics::stubs {

namespace {

using fuchsia_power_system::LeaseToken;

}  // namespace

void SystemActivityGovernor::AcquireWakeLease(AcquireWakeLeaseRequest& request,
                                              AcquireWakeLeaseCompleter::Sync& completer) {
  LeaseToken client_token, server_token;
  LeaseToken::create(/*options=*/0u, &client_token, &server_token);

  // Start an async task to wait for EVENTPAIR_PEER_CLOSED signal on server_token.
  zx_handle_t token_handle = server_token.get();
  active_wake_leases_[token_handle] = std::move(server_token);
  auto wait = std::make_unique<async::WaitOnce>(token_handle, ZX_EVENTPAIR_PEER_CLOSED);
  wait->Begin(dispatcher_, [this, token_handle, wait = std::move(wait)](
                               async_dispatcher_t*, async::WaitOnce*, zx_status_t status,
                               const zx_packet_signal_t*) {
    FX_CHECK(status == ZX_OK);
    FX_CHECK(active_wake_leases_.contains(token_handle));
    active_wake_leases_.erase(token_handle);
  });

  completer.Reply(fit::ok(std::move(client_token)));
}

void SystemActivityGovernor::GetPowerElements(GetPowerElementsCompleter::Sync& completer) {
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  fuchsia_power_system::ExecutionState execution_state;
  execution_state.opportunistic_dependency_token(std::move(event));

  fuchsia_power_system::PowerElements elements;
  elements.execution_state(std::move(execution_state));

  completer.Reply(fidl::Response<fuchsia_power_system::ActivityGovernor::GetPowerElements>(
      std::move(elements)));
}

void SystemActivityGovernorNoTokens::GetPowerElements(GetPowerElementsCompleter::Sync& completer) {
  fuchsia_power_system::PowerElements elements;
  elements.execution_state(fuchsia_power_system::ExecutionState());

  completer.Reply(fidl::Response<fuchsia_power_system::ActivityGovernor::GetPowerElements>(
      std::move(elements)));
}

}  // namespace forensics::stubs
