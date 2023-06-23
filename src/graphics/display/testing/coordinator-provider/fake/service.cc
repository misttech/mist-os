// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/coordinator-provider/fake/service.h"

#include <lib/async/cpp/task.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>

#include "src/graphics/display/drivers/fake/sysmem-proxy-device.h"

namespace display {

// static
zx::result<> FakeDisplayCoordinatorConnector::CreateAndPublishService(
    std::shared_ptr<zx_device> mock_root, async_dispatcher_t* dispatcher,
    component::OutgoingDirectory& outgoing) {
  return outgoing.AddProtocol<fuchsia_hardware_display::Provider>(
      std::make_unique<FakeDisplayCoordinatorConnector>(std::move(mock_root), dispatcher));
}

FakeDisplayCoordinatorConnector::FakeDisplayCoordinatorConnector(
    std::shared_ptr<zx_device> mock_root, async_dispatcher_t* dispatcher) {
  FX_DCHECK(dispatcher);

  auto sysmem = std::make_unique<display::GenericSysmemDeviceWrapper<display::SysmemProxyDevice>>(
      mock_root.get());
  static constexpr fake_display::FakeDisplayDeviceConfig kDeviceConfig = {
      .manual_vsync_trigger = false,
      .no_buffer_access = false,
  };
  state_ = std::shared_ptr<State>(
      new State{.dispatcher = dispatcher,
                .fake_display_stack = std::make_unique<display::FakeDisplayStack>(
                    std::move(mock_root), std::move(sysmem), kDeviceConfig)});
}

FakeDisplayCoordinatorConnector::~FakeDisplayCoordinatorConnector() {
  state_->fake_display_stack->SyncShutdown();
}

void FakeDisplayCoordinatorConnector::OpenCoordinatorForPrimary(
    OpenCoordinatorForPrimaryRequest& request,
    OpenCoordinatorForPrimaryCompleter::Sync& completer) {
  ConnectOrDeferClient(OpenCoordinatorRequest{
      .is_virtcon = false,
      .coordinator_request = std::move(request.coordinator()),
      .on_coordinator_opened =
          [async_completer = completer.ToAsync()](zx_status_t status) mutable {
            async_completer.Reply({{.s = status}});
          },
  });
}

void FakeDisplayCoordinatorConnector::OpenCoordinatorForVirtcon(
    OpenCoordinatorForVirtconRequest& request,
    OpenCoordinatorForVirtconCompleter::Sync& completer) {
  ConnectOrDeferClient(OpenCoordinatorRequest{
      .is_virtcon = true,
      .coordinator_request = std::move(request.coordinator()),
      .on_coordinator_opened = [async_completer = completer.ToAsync()](zx_status_t status) mutable {
        async_completer.Reply({{.s = status}});
      }});
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

  zx_status_t status = state->fake_display_stack->coordinator_controller()->CreateClient(
      request.is_virtcon, std::move(request.coordinator_request),
      /*on_client_dead=*/
      [state_weak_ptr, use_virtcon_coordinator]() mutable {
        std::shared_ptr<State> state = state_weak_ptr.lock();
        if (!state) {
          return;
        }
        // Redispatch `ReleaseCoordinatorAndConnectToNextQueuedClient()` back
        // to the state async dispatcher (where it is only allowed to run),
        // since the `on_client_dead` callback may not be expected to run on
        // that dispatcher.
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
