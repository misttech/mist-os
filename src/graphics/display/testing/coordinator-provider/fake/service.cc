// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/coordinator-provider/fake/service.h"

#include <lib/async/cpp/task.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include "src/graphics/display/drivers/fake/sysmem-proxy-device.h"

namespace fake_display {

ProviderService::ProviderService(std::shared_ptr<zx_device> mock_root,
                                 sys::ComponentContext* app_context,
                                 async_dispatcher_t* dispatcher) {
  FX_DCHECK(dispatcher);

  // |app_context| may be null for in-process tests.
  if (app_context) {
    app_context->outgoing()->AddPublicService(bindings_.GetHandler(this));
  }

  auto sysmem = std::make_unique<display::GenericSysmemDeviceWrapper<display::SysmemProxyDevice>>(
      mock_root.get());
  state_ = std::make_shared<State>(
      State{.dispatcher = dispatcher,
            .tree = std::make_unique<display::MockDisplayDeviceTree>(
                std::move(mock_root), std::move(sysmem), /*start_vsync=*/true)});
}

ProviderService::~ProviderService() { state_->tree->AsyncShutdown(); }

void ProviderService::OpenCoordinatorForPrimary(
    ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> coordinator_request,
    OpenCoordinatorForPrimaryCallback callback) {
  ConnectOrDeferClient(Request{.is_virtcon = false,
                               .coordinator_request = std::move(coordinator_request),
                               .callback = std::move(callback)});
}

void ProviderService::OpenCoordinatorForVirtcon(
    ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> coordinator_request,
    OpenCoordinatorForVirtconCallback callback) {
  ConnectOrDeferClient(Request{.is_virtcon = true,
                               .coordinator_request = std::move(coordinator_request),
                               .callback = std::move(callback)});
}

void ProviderService::ConnectOrDeferClient(Request req) {
  bool claimed = req.is_virtcon ? state_->virtcon_coordinator_claimed : state_->coordinator_claimed;
  if (claimed) {
    auto& queue = req.is_virtcon ? state_->virtcon_queued_requests : state_->queued_requests;
    queue.push(std::move(req));
  } else {
    ConnectClient(std::move(req), state_);
  }
}

void ProviderService::ConnectClient(Request req, const std::shared_ptr<State>& state) {
  FX_DCHECK(state);

  // Claim the connection type specified in the request, which MUST not already be claimed.
  {
    auto& claimed =
        req.is_virtcon ? state->virtcon_coordinator_claimed : state->coordinator_claimed;
    FX_CHECK(!claimed) << "coordinator already claimed.";
    claimed = true;
  }

  zx_status_t status = state->tree->coordinator_controller()->CreateClient(
      req.is_virtcon,
      fidl::ServerEnd<fuchsia_hardware_display::Coordinator>{req.coordinator_request.TakeChannel()},
      [weak = std::weak_ptr<State>(state), is_virtcon{req.is_virtcon}]() mutable {
        // Redispatch, in case this callback is invoked on a different thread (this depends
        // on the implementation of MockDisplayDeviceTree, which makes no guarantees).
        if (auto state = weak.lock()) {
          async::PostTask(state->dispatcher, [weak, is_virtcon]() mutable {
            if (auto state = weak.lock()) {
              // Obtain the claim status and queue matching the connection that was just released.
              auto& claimed =
                  is_virtcon ? state->virtcon_coordinator_claimed : state->coordinator_claimed;
              auto& queue = is_virtcon ? state->virtcon_queued_requests : state->queued_requests;

              // The connection is no longer claimed.  If there is a queued connection request of
              // the same type (i.e. virtcon or not virtcon), then establish a connection.
              claimed = false;
              if (!queue.empty()) {
                Request req = std::move(queue.front());
                queue.pop();
                ConnectClient(std::move(req), state);
              }
            }
          });
        }
      });
  req.callback(status);
}

}  // namespace fake_display
