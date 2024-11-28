// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/fake-coordinator-connector/service.h"

#include <lib/async/cpp/task.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>

#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/fake/sysmem-service-forwarder.h"

namespace display {

FakeDisplayCoordinatorConnector::FakeDisplayCoordinatorConnector(
    async_dispatcher_t* dispatcher,
    const fake_display::FakeDisplayDeviceConfig& fake_display_device_config) {
  FX_DCHECK(dispatcher);

  zx::result<std::unique_ptr<SysmemServiceForwarder>> sysmem_service_forwarder_result =
      display::SysmemServiceForwarder::Create();
  FX_CHECK(sysmem_service_forwarder_result.is_ok());

  auto fake_display_stack = std::make_unique<display::FakeDisplayStack>(
      std::move(sysmem_service_forwarder_result).value(), fake_display_device_config);
  state_ = std::shared_ptr<State>(
      new State{.dispatcher = dispatcher, .fake_display_stack = std::move(fake_display_stack)});
}

FakeDisplayCoordinatorConnector::~FakeDisplayCoordinatorConnector() {
  state_->fake_display_stack->SyncShutdown();
}

void FakeDisplayCoordinatorConnector::OpenCoordinatorWithListenerForPrimary(
    OpenCoordinatorWithListenerForPrimaryRequest& request,
    OpenCoordinatorWithListenerForPrimaryCompleter::Sync& completer) {
  ConnectOrDeferClient(OpenCoordinatorRequest{
      .is_virtcon = false,
      .coordinator_request = std::move(*request.coordinator()),
      .coordinator_listener_client_end = std::move(*request.coordinator_listener()),
      .on_coordinator_opened =
          [async_completer = completer.ToAsync()](zx_status_t status) mutable {
            if (status == ZX_OK) {
              async_completer.Reply(fit::ok());
            } else {
              async_completer.Reply(fit::error(status));
            }
          },
  });
}

void FakeDisplayCoordinatorConnector::OpenCoordinatorWithListenerForVirtcon(
    OpenCoordinatorWithListenerForVirtconRequest& request,
    OpenCoordinatorWithListenerForVirtconCompleter::Sync& completer) {
  ConnectOrDeferClient(OpenCoordinatorRequest{
      .is_virtcon = true,
      .coordinator_request = std::move(*request.coordinator()),
      .coordinator_listener_client_end = std::move(*request.coordinator_listener()),
      .on_coordinator_opened =
          [async_completer = completer.ToAsync()](zx_status_t status) mutable {
            if (status == ZX_OK) {
              async_completer.Reply(fit::ok());
            } else {
              async_completer.Reply(fit::error(status));
            }
          },
  });
}

void FakeDisplayCoordinatorConnector::ConnectOrDeferClient(OpenCoordinatorRequest req) {
  bool claimed =
      req.is_virtcon ? state_->virtcon_coordinator_claimed : state_->primary_coordinator_claimed;
  if (claimed) {
    auto& queue =
        req.is_virtcon ? state_->queued_virtcon_requests : state_->queued_primary_requests;
    queue.push(std::move(req));
  } else {
    ConnectClient(std::move(req), state_);
  }
}

// static
void FakeDisplayCoordinatorConnector::ReleaseCoordinatorAndConnectToNextQueuedClient(
    bool use_virtcon_coordinator, std::shared_ptr<State> state) {
  state->MarkCoordinatorUnclaimed(use_virtcon_coordinator);
  std::queue<OpenCoordinatorRequest>& queued_requests =
      state->GetQueuedRequests(use_virtcon_coordinator);

  // If there is a queued connection request of the same type (i.e.
  // virtcon or not virtcon), then establish a connection.
  if (!queued_requests.empty()) {
    OpenCoordinatorRequest request = std::move(queued_requests.front());
    queued_requests.pop();
    ConnectClient(std::move(request), state);
  }
}

// static
void FakeDisplayCoordinatorConnector::ConnectClient(OpenCoordinatorRequest request,
                                                    const std::shared_ptr<State>& state) {
  FX_DCHECK(state);

  bool use_virtcon_coordinator = request.is_virtcon;
  state->MarkCoordinatorClaimed(use_virtcon_coordinator);
  std::weak_ptr<State> state_weak_ptr = state;

  display_coordinator::ClientPriority client_priority =
      request.is_virtcon ? display_coordinator::ClientPriority::kVirtcon
                         : display_coordinator::ClientPriority::kPrimary;
  zx_status_t status = state->fake_display_stack->coordinator_controller()->CreateClient(
      client_priority, std::move(request.coordinator_request),
      std::move(request.coordinator_listener_client_end),
      /*on_client_disconnected=*/
      [state_weak_ptr, use_virtcon_coordinator]() mutable {
        std::shared_ptr<State> state = state_weak_ptr.lock();
        if (!state) {
          return;
        }
        // Redispatch `ReleaseCoordinatorAndConnectToNextQueuedClient()` back
        // to the state async dispatcher (where it is only allowed to run),
        // since the `on_client_disconnected` callback may not be expected to
        // run on that dispatcher.
        async::PostTask(state->dispatcher, [state_weak_ptr, use_virtcon_coordinator]() mutable {
          if (std::shared_ptr<State> state = state_weak_ptr.lock(); state) {
            ReleaseCoordinatorAndConnectToNextQueuedClient(use_virtcon_coordinator,
                                                           std::move(state));
          }
        });
      });
  request.on_coordinator_opened(status);
}

}  // namespace display
